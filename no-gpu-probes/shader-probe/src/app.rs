//! Bisect the application's compositor WGSL against a CPU/DX12-WARP device.
//!
//! The full shader hard-crashes (0xC0000005) inside `create_render_pipeline`.
//! This probe compiles progressively larger prefixes of the `apply_visual_effects`
//! switch so the exact effect case that first triggers the compiler failure is
//! identified by number.
//!
//! The shader is read straight out of `freya-nle-render/src/lib.rs` and every
//! `textureSample` is rewritten to `textureSampleLevel(..., 0.0)` first, because
//! the gradient instruction is what FXC cannot unroll in the dynamic loop.
//! Without the rewrite the small prefixes still fail on X3511 briefly, which
//! hides the size-driven crash this probe bisects.
//!
//! A hard access violation cannot be caught in-process, so each case count runs
//! in its own child process. Usage: shader-probe app <cases>

use crate::shader::compositor_shader;


use std::borrow::Cow;

/// Replaces every `textureSample(` call with `textureSampleLevel(<args>, 0.0)`,
/// collapsing multi-line calls onto one line. This is the same rewrite the
/// other compositor probes apply so each one measures the shader in the same
/// form.
fn rewrite_samples(source: &str) -> String {
    let marker = "textureSample(";
    let text: Vec<char> = source.chars().collect();
    let mut rewritten = String::with_capacity(source.len() + 512);
    let mut i = 0usize;
    let mut rewrote = 0usize;
    while i < text.len() {
        if text[i..].starts_with(&marker.chars().collect::<Vec<_>>()[..]) {
            let body = i + marker.chars().count();
            let mut depth = 1usize;
            let mut end = None;
            let mut j = body;
            while j < text.len() {
                match text[j] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            let end = end.expect("unterminated textureSample call");
            let call: String = text[i..=end].iter().collect();
            let arguments = &call[marker.len()..call.len() - 1];
            let collapsed = arguments.split_whitespace().collect::<Vec<_>>().join(" ");
            let trimmed = collapsed.trim_end().trim_end_matches(',').to_string();
            rewritten.push_str(&format!("textureSampleLevel({trimmed}, 0.0)"));
            rewrote += 1;
            i = end + 1;
        } else {
            rewritten.push(text[i]);
            i += 1;
        }
    }
    println!("REWRITE_SAMPLES {rewrote}");
    rewritten
}

fn build_shader(cases_to_keep: usize) -> String {
    let source = rewrite_samples(&compositor_shader());
    let lines: Vec<&str> = source.lines().collect();
    // Find the region of the switch inside apply_visual_effects.
    let fn_start = lines
        .iter()
        .position(|line| line.contains("fn apply_visual_effects"))
        .expect("apply_visual_effects");
    let switch_start = lines
        .iter()
        .enumerate()
        .skip(fn_start)
        .find(|(_, line)| line.contains("switch"))
        .map(|(index, _)| index)
        .expect("switch");
    let mut depth = 0i32;
    let mut switch_end = switch_start;
    for (offset, line) in lines.iter().enumerate().skip(switch_start) {
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if offset > switch_start && depth <= 0 {
            switch_end = offset;
            break;
        }
    }
    // Count the `case N:` lines inside the switch so they can be truncated.
    let mut case_positions: Vec<usize> = Vec::new();
    for (index, line) in lines.iter().enumerate().take(switch_end).skip(switch_start) {
        if line.trim_start().starts_with("case ") && line.trim_end().ends_with(": {") {
            case_positions.push(index);
        }
    }
    case_positions.push(switch_end);

    let mut output = String::new();
    for line in &lines[..switch_start] {
        output.push_str(line);
        output.push('\n');
    }
    output.push_str("    switch kind {\n");
    for (index, start) in case_positions.iter().enumerate() {
        if index >= cases_to_keep {
            break;
        }
        // `case_positions` ends with the switch's closing line as a sentinel, so a
        // count at or past the real case total must not index one past it.
        let Some(end) = case_positions.get(index + 1).copied() else {
            break;
        };
        for line in &lines[*start..end] {
            output.push_str(line);
            output.push('\n');
        }
    }
    // A synthetic `default` only needs to appear when the truncated switch no longer
    // reaches its real end: once the kept cases run to `switch_end` the original
    // closing brace has been copied out, and adding another default would be invalid.
    let kept_to_end = case_positions
        .get(cases_to_keep)
        .copied()
        .unwrap_or(switch_end)
        == switch_end;
    if !kept_to_end {
        output.push_str("        default: {}\n");
    }
    output.push_str("    }\n");
    for line in &lines[switch_end + 1..] {
        output.push_str(line);
        output.push('\n');
    }
    output
}

pub fn run() {
    let cases: usize = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .expect("usage: shader-probe app <cases>");

    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("cpu adapter");
    println!("ADAPTER {:?}", adapter.get_info());
    let (device, _queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device");

    let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let storage_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("probe-layout"),
        entries: &[
            texture_entry(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            texture_entry(3),
            storage_entry(4),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("probe-pipeline-layout"),
        bind_group_layouts: &[Some(&layer_layout)],
        immediate_size: 0,
    });

    let shader_source = build_shader(cases);
    println!("TRY cases={cases} shader_bytes={}", shader_source.len());
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("probe-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader_source)),
    });
    let _ = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("probe-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    println!("OK cases={cases}");
}
//! Is the WARP pipeline-compilation failure caused by shader sheer size, or by
//! a specific construct?
//!
//! This probe compiles shaders built from the application's real shader text
//! with the `apply_visual_effects` body progressively truncated, so the number
//! of switch cases and the byte size can be compared against the outcome.
//!
//! Usage: shader-probe fragment <cases>

use crate::shader::compositor_shader;


use std::borrow::Cow;

fn build_shader(cases_to_keep: usize) -> String {
    let source = compositor_shader();
    let lines: Vec<&str> = source.lines().collect();
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
        .expect("usage: shader-probe fragment <cases>");
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("cpu adapter");
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
    let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("probe-layer-layout"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("probe-pipeline-layout"),
        bind_group_layouts: &[Some(&layer_layout)],
        immediate_size: 0,
    });
    println!("LAYOUT_OK cases={cases}");

    let source = build_shader(cases);
    println!("SOURCE_BYTES {}", source.len());
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("probe-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(source)),
    });
    println!("SHADER_OK");
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
    println!("RESULT compiled");
}

//! Render the Freya NLE Slint windows without a window server or GPU.
//!
//! This exists so a layout change can be reviewed as an image instead of by
//! launching the editor. It uses Slint's software renderer and writes raw RGBA
//! frames; `tools/ui-snapshot/snapshot.py` converts them to PNG.
//!
//! Usage: `cargo run -- timeline` or `cargo run -- main`.

slint::include_modules!();

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{PlatformError, WindowAdapter};
use slint::{Color, ModelRc, PhysicalSize, VecModel};
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static WINDOW: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
}

struct OffscreenPlatform;

impl slint::platform::Platform for OffscreenPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(WINDOW.with(|window| window.clone()))
    }
}

fn main() {
    slint::platform::set_platform(Box::new(OffscreenPlatform))
        .expect("install the offscreen platform");

    let which = std::env::args().nth(1).unwrap_or_else(|| "timeline".into());
    // The editor follows the OS; for review both palettes are rendered
    // explicitly. `UI_SNAPSHOT_THEME=light` selects the light one.
    let preference = match std::env::var("UI_SNAPSHOT_THEME").as_deref() {
        Ok("light") => 1,
        _ => 0,
    };
    let (width, height) = match which.as_str() {
        "main" => (640u32, 400u32),
        "timeline" => (1280u32, 460u32),
        other => {
            eprintln!("unknown window {other:?}; expected `timeline` or `main`");
            std::process::exit(2);
        }
    };

    let window = WINDOW.with(|window| window.clone());
    window.set_size(PhysicalSize::new(width, height));

    match which.as_str() {
        "main" => {
            let ui = MainWindow::new().expect("create the main window");
            ui.global::<AppTheme>().set_preference_index(preference);
            populate_main(&ui);
            ui.show().expect("show the main window");
            save(&window, width, height, &output_name("main", preference));
        }
        _ => {
            let ui = TimelineWindow::new().expect("create the timeline window");
            ui.global::<AppTheme>().set_preference_index(preference);
            populate_timeline(&ui);
            ui.show().expect("show the timeline window");
            save(&window, width, height, &output_name("timeline", preference));
        }
    }
}

fn output_name(window: &str, preference: i32) -> String {
    if preference == 1 {
        format!("{window}-light")
    } else {
        window.to_owned()
    }
}

fn save(window: &Rc<MinimalSoftwareWindow>, width: u32, height: u32, name: &str) {
    // A component needs a few event-loop turns before its layout and images
    // settle; the first frames can otherwise be blank.
    for _ in 0..6 {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|_renderer| {});
        std::thread::sleep(Duration::from_millis(16));
    }

    let mut buffer = vec![PremultipliedRgbaColor::default(); (width * height) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(buffer.as_mut_slice(), width as usize);
    });

    // The renderer paints premultiplied alpha; PNG wants straight RGBA.
    let mut rgba = Vec::with_capacity(buffer.len() * 4);
    for pixel in &buffer {
        let alpha = pixel.alpha;
        let unp = |channel: u8| {
            if alpha == 0 {
                0
            } else {
                ((u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255)
                    as u8
            }
        };
        rgba.extend_from_slice(&[unp(pixel.red), unp(pixel.green), unp(pixel.blue), alpha]);
    }

    let path = format!("/tmp/{name}.rgba");
    std::fs::write(&path, rgba).expect("write the RGBA frame");
    std::fs::write(format!("/tmp/{name}.size"), format!("{width} {height}")).expect("write size");
    println!("{name}\t{width}x{height}\t{path}");
}

fn populate_main(ui: &MainWindow) {
    ui.set_project_tabs(ModelRc::new(VecModel::from(vec![
        ProjectTabData {
            name: "harbor_trip".into(),
            dirty: false,
        },
        ProjectTabData {
            name: "title_reel".into(),
            dirty: true,
        },
    ])));
    ui.set_current_project(0);
    ui.set_playhead(240.0);
    ui.set_duration(1200.0);
    ui.set_playhead_frame(240);
    ui.set_frame_rate(30);
    ui.set_frame_counter_text("0240 / 1200".into());
    ui.set_status_text("Relinked «ambience_rain.wav» · 12 effects applied".into());
    ui.set_playback_speed_multiplier("1.0x".into());
    ui.set_audio_peak_left(0.62);
    ui.set_audio_peak_right(0.48);
    ui.set_audio_rms_left(0.35);
    ui.set_audio_rms_right(0.28);
}

fn populate_timeline(ui: &TimelineWindow) {
    ui.set_scene_tabs(ModelRc::new(VecModel::from(vec![
        SceneTabData {
            id: 1,
            name: "Root".into(),
            selected: true,
            root: true,
        },
        SceneTabData {
            id: 2,
            name: "Scene 2".into(),
            selected: false,
            root: false,
        },
    ])));
    ui.set_layers(ModelRc::new(VecModel::from(
        (0..14)
            .map(|index| LayerData {
                index,
                visible: index != 5,
                locked: index == 6,
                selected: index == 0,
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_clips(ModelRc::new(VecModel::from(vec![
        clip(
            1,
            "harbor_sunrise.mp4",
            "video",
            Color::from_rgb_u8(0x30, 0x40, 0xe0),
            0,
            210,
            0,
            false,
        ),
        clip(
            2,
            "「海边的清晨」",
            "text",
            Color::from_rgb_u8(0xc8, 0xa0, 0x30),
            60,
            90,
            1,
            false,
        ),
        clip(
            3,
            "camera_control",
            "control",
            Color::from_rgb_u8(0x30, 0xb0, 0xc0),
            120,
            150,
            2,
            false,
        ),
        clip(
            4,
            "ambience_rain.wav",
            "audio",
            Color::from_rgb_u8(0xd0, 0x40, 0x30),
            30,
            300,
            3,
            true,
        ),
        clip(
            5,
            "blur (filter)",
            "filter",
            Color::from_rgb_u8(0x30, 0xb0, 0x30),
            260,
            70,
            1,
            false,
        ),
        clip(
            6,
            "title_card.png",
            "video",
            Color::from_rgb_u8(0x58, 0x68, 0xff),
            360,
            150,
            2,
            false,
        ),
    ])));
    ui.set_duration(600);
    ui.set_playhead(240);
    ui.set_selected_layer(0);
    ui.set_selected_clip_count(1);
    ui.set_pixels_per_frame(1.6);
    ui.set_scene_name("Root".into());
    ui.set_scene_resolution("1920x1080".into());
    ui.set_scene_frame_rate(30);
    ui.set_grid_fps(30.0);
    ui.set_media_bin_visible(true);
    ui.set_media_filter("".into());
    ui.set_media_group_filter(-2);
    ui.set_media_group_names(ModelRc::new(VecModel::from(vec![
        "All media".into(),
        "Ungrouped".into(),
    ])));
    ui.set_media_entries(ModelRc::new(VecModel::from(vec![
        media(1, "harbor_sunrise.mp4", "video", 600, false),
        media(2, "ambience_rain.wav", "audio", 900, true),
        media(3, "title_card.png", "image", 120, false),
    ])));
    ui.set_selected_media_id(2);
}

fn clip(
    id: i32,
    label: &str,
    clip_type: &str,
    color: Color,
    start: i32,
    duration: i32,
    layer: i32,
    audio: bool,
) -> TimelineClipData {
    TimelineClipData {
        id,
        label: label.into(),
        clip_type: clip_type.into(),
        clip_color: color,
        has_clip_color: true,
        dark_text: false,
        waveform: if audio {
            ModelRc::new(VecModel::from(
                (0..64)
                    .map(|index| ((index as f32 * 0.7).sin().abs() * 0.8).max(0.08))
                    .collect::<Vec<f32>>(),
            ))
        } else {
            ModelRc::new(VecModel::<f32>::default())
        },
        start,
        duration,
        layer,
        audio,
        locked: false,
        control_layer_count: if clip_type == "control" { 2 } else { 0 },
        clip_by_upper_object: false,
        selected: id == 1,
        primary: id == 1,
        midpoints: ModelRc::new(VecModel::<i32>::default()),
    }
}

fn media(
    id: i32,
    name: &str,
    kind: &str,
    duration_frames: i32,
    selected: bool,
) -> MediaBinEntryData {
    MediaBinEntryData {
        id,
        path: format!("/media/{name}").into(),
        name: name.into(),
        kind: kind.into(),
        group_id: -1,
        group_name: "".into(),
        group_index: 0,
        duration_frames,
        in_frame: 0,
        out_frame: duration_frames,
        selected,
    }
}

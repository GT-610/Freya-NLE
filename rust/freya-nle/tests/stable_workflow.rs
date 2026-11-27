use freya_nle_app::ProjectSession;
use freya_nle_export::{
    ExportEvent, ExportImageFormat, ExportManager, ExportMode, ExportProject, ExportRequest,
};
use freya_nle_preview::PreviewPlanner;
use serde_json::json;
use slint::wgpu_29::wgpu;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn run(program: &str, args: &[&str]) -> Output {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("required command `{program}` could not run: {error}"));
    assert!(
        output.status.success(),
        "`{program} {}` failed ({}):\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn write_generated_media(media: &Path) {
    let video = media.join("picture.mp4");
    let audio = media.join("tone.wav");
    let video = video.to_string_lossy().into_owned();
    let audio = audio.to_string_lossy().into_owned();
    run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=160x90:r=24:d=0.5",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=sample_rate=48000:channel_layout=stereo:d=0.5",
            "-c:a",
            "aac",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-y",
            &video,
        ],
    );
    run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.5",
            "-c:a",
            "pcm_s16le",
            "-y",
            &audio,
        ],
    );
}

fn sample_project_json() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "version": 3,
        "settings": {"width": 160, "height": 90, "fps": 24, "sampleRate": 48000},
        "scenes": [{"id": 1, "name": "Root", "width": 160, "height": 90,
            "fps": 24, "duration": 12}],
        "clips": [
            {"id": 1, "sceneId": 1, "type": "video", "start": 0,
                "duration": 12, "layer": 0,
                "effects": [{"id": "video", "enabled": true,
                    "params": {"path": "media/picture.mp4"}}]},
            {"id": 2, "sceneId": 1, "type": "audio", "start": 0,
                "duration": 12, "layer": 1,
                "effects": [{"id": "audio", "enabled": true,
                    "params": {"source": "media/tone.wav", "volume": 1.0}}]}
        ]
    }))
    .expect("acceptance project serializes")
}

fn assert_external_decode(output: &Path) {
    let path = output.to_string_lossy().into_owned();
    let streams = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
            &path,
        ],
    );
    let streams = String::from_utf8_lossy(&streams.stdout);
    assert!(
        streams.lines().any(|line| line.trim() == "video"),
        "{streams}"
    );
    assert!(
        streams.lines().any(|line| line.trim() == "audio"),
        "{streams}"
    );

    let frame = run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &path,
            "-map",
            "0:v:0",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ],
    );
    const RGB_FRAME_BYTES: usize = 160 * 90 * 3;
    let (frames, trailing_bytes) = frame.stdout.as_chunks::<RGB_FRAME_BYTES>();
    assert!(trailing_bytes.is_empty());
    assert_eq!(frames.len(), 12);
    for (index, frame) in frames.iter().enumerate() {
        let (pixels, _) = frame.as_chunks::<3>();
        let red_pixels = pixels
            .iter()
            .filter(|pixel| pixel[0] > 160 && pixel[1] < 100 && pixel[2] < 100)
            .count();
        assert!(
            red_pixels > 160 * 90 / 2,
            "frame {index}: only {red_pixels} red pixels"
        );
    }

    let audio = run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &path,
            "-map",
            "0:a:0",
            "-f",
            "f32le",
            "-ac",
            "1",
            "-ar",
            "48000",
            "pipe:1",
        ],
    );
    let (samples, trailing_bytes) = audio.stdout.as_chunks::<4>();
    assert!(trailing_bytes.is_empty());
    let peak = samples
        .iter()
        .map(|sample| f32::from_le_bytes(*sample).abs())
        .fold(0.0_f32, f32::max);
    assert!(peak > 0.01, "decoded audio is silent (peak {peak})");
}

#[test]
#[ignore = "requires a non-CPU wgpu adapter, FFmpeg CLI (libx264), ffprobe, and linked H.264/AAC encoders"]
fn saves_reopens_resolves_relative_media_renders_and_exports_playable_mp4() {
    let video_encoders = freya_nle_media::available_video_encoders();
    let audio_encoders = freya_nle_media::available_audio_encoders();
    assert!(
        video_encoders.contains(&"libx264"),
        "linked FFmpeg has no libx264 encoder: {video_encoders:?}"
    );
    assert!(
        audio_encoders.contains(&"aac"),
        "linked FFmpeg has no AAC encoder: {audio_encoders:?}"
    );
    run("ffmpeg", &["-version"]);
    run("ffprobe", &["-version"]);

    let directory = tempfile::tempdir().expect("acceptance directory creates");
    let project_dir = directory.path().join("original project");
    let media = project_dir.join("media");
    std::fs::create_dir_all(&media).expect("media directory creates");
    write_generated_media(&media);

    let mut project = ProjectSession::from_json(&sample_project_json()).expect("project parses");
    project
        .save_as(&project_dir.join("project.freya"))
        .expect("project saves");
    let moved_dir = directory.path().join("moved project");
    std::fs::rename(project_dir, &moved_dir).expect("project and media move together");
    let project_path = moved_dir.join("project.freya");
    let media = moved_dir.join("media");
    let project = ProjectSession::load(&project_path).expect("moved project reopens");
    assert!(!project.dirty);
    assert_eq!(project.document.clips.len(), 2);
    assert_eq!(
        project.document.clips[0].effects[0].params["path"],
        "media/picture.mp4"
    );
    assert_eq!(
        freya_nle_app::missing_media::resolve_path(Some(&project_path), "media/picture.mp4"),
        media.join("picture.mp4")
    );
    assert!(media.join("picture.mp4").is_file());

    let mut planner = PreviewPlanner::new(&project.document, Some(&project_path));
    let planned = planner
        .build(&project.document, 1, 0)
        .expect("root preview plans");
    assert!(
        planned.warnings.is_empty(),
        "preview warnings: {:?}",
        planned.warnings
    );
    assert_eq!(
        planned.scene.layers.len(),
        1,
        "video media should be in the rendered scene"
    );
    let audio_sources: Vec<_> = planned
        .audio
        .iter()
        .filter(|layer| layer.source_path.is_some())
        .map(|layer| layer.clip_id)
        .collect();
    assert!(
        audio_sources.contains(&2),
        "standalone audio was not planned: {audio_sources:?}"
    );

    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("a wgpu adapter is required for this acceptance test");
    let info = adapter.get_info();
    eprintln!(
        "stable-workflow GPU: {} ({:?}, {:?})",
        info.name, info.backend, info.device_type
    );
    assert_ne!(
        info.device_type,
        wgpu::DeviceType::Cpu,
        "CPU adapter is not GPU acceptance"
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("stable-workflow-acceptance"),
        ..Default::default()
    }))
    .expect("wgpu device creation succeeds");
    let mut manager = ExportManager::new(device, queue, Arc::new(|| {}));
    let output_path = directory.path().join("result.mp4");
    assert_eq!(
        manager
            .start(
                1,
                ExportProject {
                    width: 160,
                    height: 90,
                    fps: 24.0,
                    duration: 12,
                    sample_rate: 48_000,
                },
                ExportRequest {
                    mode: ExportMode::Video,
                    output_path: output_path.clone(),
                    image_format: ExportImageFormat::Png,
                    video_codec: "libx264".to_owned(),
                    crf: Some(20),
                    bitrate_mbps: 2,
                    preset: "ultrafast".to_owned(),
                    profile: String::new(),
                    audio_codec: "aac".to_owned(),
                    audio_bitrate_kbps: 128,
                    full_range: true,
                    start_frame: 0,
                    end_frame: 12,
                },
                Default::default(),
            )
            .expect("export starts"),
        12
    );

    let deadline = Instant::now() + Duration::from_secs(45);
    let mut finished = None;
    while manager.is_exporting() {
        if let Some(request) = manager.next_frame_request() {
            let frame = planner
                .build(&project.document, 1, request.frame)
                .unwrap_or_else(|| panic!("preview planning failed at frame {}", request.frame));
            assert!(
                frame.warnings.is_empty(),
                "frame {} warnings: {:?}",
                request.frame,
                frame.warnings
            );
            manager.submit_scene(frame.scene, frame.audio, Some(&project_path));
        }
        for event in manager.poll() {
            if let ExportEvent::Finished(result) = event {
                finished = Some(result);
            }
        }
        assert!(
            Instant::now() < deadline,
            "GPU export did not finish within 45 seconds"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let result = finished.expect("export worker reports completion");
    assert!(result.success, "export failed: {}", result.message);
    assert!(
        result.notice.is_none(),
        "unexpected export notice: {:?}",
        result.notice
    );
    assert_external_decode(&output_path);
}

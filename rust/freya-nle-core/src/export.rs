use std::path::PathBuf;

const DEFAULT_VIDEO_CODEC: &str = "libx264";
const DEFAULT_AUDIO_CODEC: &str = "aac";
const MIN_ENCODER_QUEUE_TASKS: u128 = 2;
const MAX_ENCODER_QUEUE_TASKS: u128 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportConfigurationError {
    MissingOutputPath,
    InvalidOutputSize,
    InvalidFps,
    InvalidRange,
    ProjectFpsMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportImageFormat {
    Png,
    Jpeg,
}

impl ExportImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportCodecBackend {
    Software,
    Cuda,
    Vaapi,
    Qsv,
    D3d11va,
    Dxva2,
    VideoToolbox,
    Amf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFixedGopMode {
    None,
    X264,
    X265,
    Nvenc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoExportDefaults {
    pub width: i32,
    pub height: i32,
    pub fps_num: i32,
    pub fps_den: i32,
    pub bitrate: i64,
    pub crf: i32,
    pub gop_size: i32,
    pub audio_bitrate: i64,
    pub start_frame: i32,
    pub end_frame: i32,
    pub video_codec: &'static str,
    pub audio_codec: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoExportRequest {
    pub width: i32,
    pub height: i32,
    pub fps_num: i32,
    pub fps_den: i32,
    pub start_frame: i32,
    pub end_frame: i32,
    pub timeline_duration: i32,
    pub output_path: PathBuf,
    pub project_fps: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoExportPlan {
    pub start_frame: i32,
    pub end_frame: i32,
    pub total_frames: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSequenceExportRequest {
    pub start_frame: i32,
    pub end_frame: i32,
    pub timeline_duration: i32,
    pub configured_padding: i32,
    pub output_directory: PathBuf,
    pub format: ExportImageFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSequenceExportPlan {
    pub start_frame: i32,
    pub end_frame: i32,
    pub total_frames: i32,
    pub pad_digits: i32,
    pub format: ExportImageFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportAudioFramePlan {
    pub cumulative_samples: i64,
    pub samples_for_frame: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportProgressPlan {
    pub progress: i32,
    pub current_frame: i32,
    pub total_frames: i32,
    pub eta_seconds: i32,
    pub should_emit: bool,
}

pub fn video_export_defaults() -> VideoExportDefaults {
    video_defaults()
}

pub fn plan_video_export(
    request: &VideoExportRequest,
) -> Result<VideoExportPlan, ExportConfigurationError> {
    plan_video(request)
}

pub fn plan_image_sequence_export(
    request: &ImageSequenceExportRequest,
) -> Result<ImageSequenceExportPlan, ExportConfigurationError> {
    plan_image_sequence(request)
}

pub fn plan_export_audio_frame(
    frame_index: i32,
    sample_rate: i32,
    fps_num: i32,
    fps_den: i32,
) -> Option<ExportAudioFramePlan> {
    plan_audio_frame(frame_index, sample_rate, fps_num, fps_den)
}

pub fn plan_export_progress(
    done: i32,
    total_frames: i32,
    interval: i32,
    elapsed_ms: i64,
) -> Option<ExportProgressPlan> {
    plan_progress(done, total_frames, interval, elapsed_ms)
}

pub fn export_codec_backend(codec_name: &str) -> ExportCodecBackend {
    codec_backend(codec_name)
}

pub fn export_codec_fallback(codec_name: &str) -> Option<&'static str> {
    codec_fallback(codec_name)
}

pub fn export_fixed_gop_mode(codec_name: &str) -> ExportFixedGopMode {
    fixed_gop_mode(codec_name)
}

pub fn export_encoder_queue_size(width: i32, height: i32, budget_mb: i32) -> usize {
    encoder_queue_size(width, height, budget_mb)
}

fn video_defaults() -> VideoExportDefaults {
    VideoExportDefaults {
        width: 1920,
        height: 1080,
        fps_num: 60_000,
        fps_den: 1_000,
        bitrate: 15_000_000,
        crf: -1,
        gop_size: 0,
        audio_bitrate: 192_000,
        start_frame: 0,
        end_frame: -1,
        video_codec: DEFAULT_VIDEO_CODEC,
        audio_codec: DEFAULT_AUDIO_CODEC,
    }
}

fn plan_video(request: &VideoExportRequest) -> Result<VideoExportPlan, ExportConfigurationError> {
    let end_frame = if request.end_frame >= 0 {
        request.end_frame
    } else {
        request.timeline_duration
    };
    if request.output_path.as_os_str().is_empty() {
        return Err(ExportConfigurationError::MissingOutputPath);
    }
    if request.width <= 0 || request.height <= 0 {
        return Err(ExportConfigurationError::InvalidOutputSize);
    }
    if request.fps_num <= 0 || request.fps_den <= 0 {
        return Err(ExportConfigurationError::InvalidFps);
    }
    if request.start_frame < 0 || end_frame <= request.start_frame {
        return Err(ExportConfigurationError::InvalidRange);
    }
    let export_fps = f64::from(request.fps_num) / f64::from(request.fps_den);
    if !request.project_fps.is_finite()
        || request.project_fps <= 0.0
        || (export_fps - request.project_fps).abs() > 0.001
    {
        return Err(ExportConfigurationError::ProjectFpsMismatch);
    }
    Ok(VideoExportPlan {
        start_frame: request.start_frame,
        end_frame,
        total_frames: end_frame - request.start_frame,
    })
}

fn decimal_digits(value: i32) -> i32 {
    let mut value = value.max(0);
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn plan_image_sequence(
    request: &ImageSequenceExportRequest,
) -> Result<ImageSequenceExportPlan, ExportConfigurationError> {
    let end_frame = if request.end_frame >= 0 {
        request.end_frame
    } else {
        request.timeline_duration
    };
    if request.output_directory.as_os_str().is_empty() {
        return Err(ExportConfigurationError::MissingOutputPath);
    }
    if request.start_frame < 0 || end_frame <= request.start_frame {
        return Err(ExportConfigurationError::InvalidRange);
    }
    let configured_padding = request.configured_padding.clamp(2, 10);
    let pad_digits = configured_padding.max(decimal_digits(end_frame.saturating_sub(1)));
    Ok(ImageSequenceExportPlan {
        start_frame: request.start_frame,
        end_frame,
        total_frames: end_frame - request.start_frame,
        pad_digits,
        format: request.format,
    })
}

fn cumulative_samples(
    frame_count: i32,
    sample_rate: i32,
    fps_num: i32,
    fps_den: i32,
) -> Option<i64> {
    let numerator = i128::from(frame_count)
        .checked_mul(i128::from(sample_rate))?
        .checked_mul(i128::from(fps_den))?;
    let value = numerator / i128::from(fps_num);
    i64::try_from(value).ok()
}

fn plan_audio_frame(
    frame_index: i32,
    sample_rate: i32,
    fps_num: i32,
    fps_den: i32,
) -> Option<ExportAudioFramePlan> {
    if frame_index < 0 || sample_rate <= 0 || fps_num <= 0 || fps_den <= 0 {
        return None;
    }
    let current = cumulative_samples(frame_index, sample_rate, fps_num, fps_den)?;
    let next = cumulative_samples(frame_index.checked_add(1)?, sample_rate, fps_num, fps_den)?;
    Some(ExportAudioFramePlan {
        cumulative_samples: next,
        samples_for_frame: i32::try_from(next.checked_sub(current)?).ok()?,
    })
}

fn plan_progress(
    done: i32,
    total_frames: i32,
    interval: i32,
    elapsed_ms: i64,
) -> Option<ExportProgressPlan> {
    if done <= 0 || total_frames <= 0 || done > total_frames || elapsed_ms < 0 {
        return None;
    }
    let interval = interval.max(1);
    let remaining_frames = i128::from(total_frames - done);
    let eta_seconds =
        i128::from(elapsed_ms).checked_mul(remaining_frames)? / i128::from(done) / 1_000;
    Some(ExportProgressPlan {
        progress: i32::try_from(i64::from(done) * 100 / i64::from(total_frames)).ok()?,
        current_frame: done,
        total_frames,
        eta_seconds: i32::try_from(eta_seconds.min(i128::from(i32::MAX))).ok()?,
        should_emit: done % interval == 0 || done == total_frames,
    })
}

fn codec_backend(codec_name: &str) -> ExportCodecBackend {
    if codec_name.contains("nvenc") {
        ExportCodecBackend::Cuda
    } else if codec_name.contains("vaapi") {
        ExportCodecBackend::Vaapi
    } else if codec_name.contains("qsv") {
        ExportCodecBackend::Qsv
    } else if codec_name.contains("d3d11") {
        ExportCodecBackend::D3d11va
    } else if codec_name.contains("dxva2") {
        ExportCodecBackend::Dxva2
    } else if codec_name.contains("videotoolbox") {
        ExportCodecBackend::VideoToolbox
    } else if codec_name.contains("amf") {
        ExportCodecBackend::Amf
    } else {
        ExportCodecBackend::Software
    }
}

fn codec_fallback(codec_name: &str) -> Option<&'static str> {
    match codec_name {
        "h264_nvenc" | "h264_amf" | "h264_qsv" | "h264_vaapi" | "h264_videotoolbox" => {
            Some("libx264")
        }
        "hevc_nvenc" | "hevc_amf" | "hevc_qsv" | "hevc_vaapi" | "hevc_videotoolbox" => {
            Some("libx265")
        }
        "av1_nvenc" | "av1_amf" | "av1_vaapi" => Some("libaom-av1"),
        _ => None,
    }
}

fn fixed_gop_mode(codec_name: &str) -> ExportFixedGopMode {
    match codec_name {
        "libx264" => ExportFixedGopMode::X264,
        "libx265" => ExportFixedGopMode::X265,
        value if value.contains("nvenc") => ExportFixedGopMode::Nvenc,
        _ => ExportFixedGopMode::None,
    }
}

fn encoder_queue_size(width: i32, height: i32, budget_mb: i32) -> usize {
    let frame_bytes = (width.max(1) as u128) * (height.max(1) as u128) * 4;
    let budget_bytes = (budget_mb.max(16) as u128) * 1024 * 1024;
    let tasks =
        (budget_bytes / frame_bytes).clamp(MIN_ENCODER_QUEUE_TASKS, MAX_ENCODER_QUEUE_TASKS);
    tasks as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn video_request() -> VideoExportRequest {
        VideoExportRequest {
            width: 1920,
            height: 1080,
            fps_num: 60_000,
            fps_den: 1_001,
            start_frame: 10,
            end_frame: -1,
            timeline_duration: 110,
            output_path: PathBuf::from("/tmp/export.mp4"),
            project_fps: 60_000.0 / 1_001.0,
        }
    }

    #[test]
    fn supplies_the_existing_export_defaults() {
        assert_eq!(
            video_defaults(),
            VideoExportDefaults {
                width: 1920,
                height: 1080,
                fps_num: 60_000,
                fps_den: 1_000,
                bitrate: 15_000_000,
                crf: -1,
                gop_size: 0,
                audio_bitrate: 192_000,
                start_frame: 0,
                end_frame: -1,
                video_codec: "libx264",
                audio_codec: "aac",
            }
        );
    }

    #[test]
    fn validates_and_resolves_video_ranges() {
        let request = video_request();
        assert_eq!(
            plan_video(&request),
            Ok(VideoExportPlan {
                start_frame: 10,
                end_frame: 110,
                total_frames: 100,
            })
        );
        assert_eq!(
            plan_video(&VideoExportRequest {
                output_path: PathBuf::new(),
                ..request.clone()
            }),
            Err(ExportConfigurationError::MissingOutputPath)
        );
        assert_eq!(
            plan_video(&VideoExportRequest {
                width: 0,
                ..request.clone()
            }),
            Err(ExportConfigurationError::InvalidOutputSize)
        );
        assert_eq!(
            plan_video(&VideoExportRequest {
                fps_den: 0,
                ..request.clone()
            }),
            Err(ExportConfigurationError::InvalidFps)
        );
        assert_eq!(
            plan_video(&VideoExportRequest {
                start_frame: 110,
                ..request.clone()
            }),
            Err(ExportConfigurationError::InvalidRange)
        );
        assert_eq!(
            plan_video(&VideoExportRequest {
                project_fps: 24.0,
                ..request
            }),
            Err(ExportConfigurationError::ProjectFpsMismatch)
        );
    }

    #[test]
    fn plans_image_format_padding_and_range() {
        let request = ImageSequenceExportRequest {
            start_frame: 3,
            end_frame: -1,
            timeline_duration: 12_345,
            configured_padding: 4,
            output_directory: PathBuf::from("/tmp/frames"),
            format: ExportImageFormat::Jpeg,
        };
        assert_eq!(
            plan_image_sequence(&request),
            Ok(ImageSequenceExportPlan {
                start_frame: 3,
                end_frame: 12_345,
                total_frames: 12_342,
                pad_digits: 5,
                format: ExportImageFormat::Jpeg,
            })
        );
        assert_eq!(
            plan_image_sequence(&ImageSequenceExportRequest {
                configured_padding: 99,
                ..request.clone()
            })
            .unwrap()
            .pad_digits,
            10
        );
        assert_eq!(
            plan_image_sequence(&ImageSequenceExportRequest {
                configured_padding: 0,
                end_frame: 5,
                ..request.clone()
            })
            .unwrap()
            .pad_digits,
            2
        );
        assert_eq!(
            plan_image_sequence(&ImageSequenceExportRequest {
                output_directory: PathBuf::new(),
                ..request
            }),
            Err(ExportConfigurationError::MissingOutputPath)
        );
    }

    #[test]
    fn audio_plan_uses_the_exact_rational_frame_rate() {
        let first = plan_audio_frame(0, 48_000, 60_000, 1_001).unwrap();
        assert_eq!(first.samples_for_frame, 800);
        assert_eq!(first.cumulative_samples, 800);

        let second = plan_audio_frame(1, 48_000, 60_000, 1_001).unwrap();
        assert_eq!(second.samples_for_frame, 801);
        assert_eq!(second.cumulative_samples, 1_601);

        let one_minute = plan_audio_frame(3_595, 48_000, 60_000, 1_001).unwrap();
        assert_eq!(one_minute.cumulative_samples, 2_879_676);
    }

    #[test]
    fn progress_plan_preserves_interval_percent_and_eta_rules() {
        assert_eq!(
            plan_progress(5, 12, 5, 2_500).unwrap(),
            ExportProgressPlan {
                progress: 41,
                current_frame: 5,
                total_frames: 12,
                eta_seconds: 3,
                should_emit: true,
            }
        );
        assert!(!plan_progress(6, 12, 5, 3_000).unwrap().should_emit);
        assert!(plan_progress(12, 12, 5, 6_000).unwrap().should_emit);
        assert_eq!(plan_progress(0, 12, 5, 3_000), None);
        assert_eq!(plan_progress(13, 12, 5, 3_000), None);
    }

    #[test]
    fn classifies_codecs_and_queue_budget() {
        assert_eq!(codec_backend("h264_nvenc"), ExportCodecBackend::Cuda);
        assert_eq!(
            codec_backend("hevc_videotoolbox"),
            ExportCodecBackend::VideoToolbox
        );
        assert_eq!(codec_backend("h264_amf"), ExportCodecBackend::Amf);
        assert_eq!(codec_backend("libx264"), ExportCodecBackend::Software);
        assert_eq!(codec_fallback("hevc_qsv"), Some("libx265"));
        assert_eq!(codec_fallback("h264_videotoolbox"), Some("libx264"));
        assert_eq!(codec_fallback("hevc_videotoolbox"), Some("libx265"));
        assert_eq!(codec_fallback("libx264"), None);
        assert_eq!(fixed_gop_mode("libx264"), ExportFixedGopMode::X264);
        assert_eq!(fixed_gop_mode("h264_nvenc"), ExportFixedGopMode::Nvenc);
        assert_eq!(fixed_gop_mode("h264_vaapi"), ExportFixedGopMode::None);
        assert_eq!(export_encoder_queue_size(1920, 1080, 128), 16);
        assert_eq!(export_encoder_queue_size(7680, 4320, 16), 2);
    }
}

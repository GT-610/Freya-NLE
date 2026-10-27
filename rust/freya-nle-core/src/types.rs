//! Plain data shared by the timeline, bake and audio pipelines.

#[derive(Clone, Copy)]
pub struct EasingParameters {
    pub amplitude: f64,
    pub period: f64,
}

#[derive(Clone, Copy, Default)]
pub struct AudioMixParameters {
    pub relative_time: f64,
    pub duration: f64,
    pub fade_in_seconds: f32,
    pub fade_out_seconds: f32,
    pub volume: f32,
    pub master_volume: f32,
    pub pan: f32,
    pub limiter: u32,
}

#[derive(Clone, Copy, Default)]
pub struct AudioMeter {
    pub peak_left: f32,
    pub peak_right: f32,
    pub rms_left: f32,
    pub rms_right: f32,
}

#[derive(Clone, Copy, Default)]
pub struct RenderBakeInput {
    pub has_transform: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub scale: f32,
    pub opacity: f32,
}

#[derive(Clone, Copy, Default)]
pub struct RenderBakeOutput {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub scale_x: f32,
    pub opacity: f32,
}

#[derive(Clone, Copy, Default)]
pub struct AudioBakeInput {
    pub clip_id: i32,
    pub start_frame: i32,
    pub duration_frames: i32,
    pub has_audio_effect: u32,
    pub fps: f64,
    pub source_start_time: f32,
    pub speed_percent: f32,
    pub direct_time: f32,
    pub volume: f32,
    pub master_volume: f32,
    pub pan: f32,
    pub fade_in_seconds: f32,
    pub fade_out_seconds: f32,
    pub direct_mode: u32,
    pub mute: u32,
    pub solo: u32,
    pub limiter: u32,
}

#[derive(Clone, Copy, Default)]
pub struct AudioBakeOutput {
    pub clip_id: i32,
    pub start_frame: i32,
    pub duration_frames: i32,
    pub source_start_time: f32,
    pub playback_speed: f32,
    pub direct_time: f32,
    pub volume: f32,
    pub master_volume: f32,
    pub pan: f32,
    pub fade_in_seconds: f32,
    pub fade_out_seconds: f32,
    pub mute: u32,
    pub solo: u32,
    pub limiter: u32,
    pub direct_mode: u32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct TimelineClipGeometry {
    pub clip_id: i32,
    pub layer: i32,
    pub start_frame: i32,
    pub duration_frames: i32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct TimelineMoveInput {
    pub clip_id: i32,
    pub old_layer: i32,
    pub old_start_frame: i32,
    pub duration_frames: i32,
    pub target_layer: i32,
    pub target_start_frame: i32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct SceneSettings {
    pub width: i32,
    pub height: i32,
    pub fps: f64,
    pub total_frames: i32,
    pub grid_mode: u32,
    pub grid_bpm: f64,
    pub grid_offset: f64,
    pub grid_interval: i32,
    pub grid_subdivision: i32,
    pub enable_snap: u32,
    pub magnetic_snap_range: i32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct IdAllocation {
    pub allocated_id: i32,
    pub next_id: i32,
}

//! Timeline-synchronous audio decoding and mixing independent of an output device.

mod output;
mod plugin_host;

pub use freya_nle_core::api::{AudioLayerPlan, StereoTrackMeter};
use freya_nle_core::api::{
    StereoMixParameters, StereoMixTrack, mix_stereo_tracks, resample_stereo,
};
use freya_nle_media::AudioDecoder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub use output::AudioOutput;
use plugin_host::{AudioPluginChain, AudioPluginProcessContext};
pub use plugin_host::{
    AudioPluginDescription, AudioPluginParameterInfo, ModernAudioPluginFormat,
    ModernAudioPluginInfo, inspect_audio_plugin, scan_modern_audio_plugins,
};

/// A resolved media path paired with its evaluated Rust timeline plan.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineAudioSource {
    pub path: PathBuf,
    pub plan: AudioLayerPlan,
}

/// One frame-sized block ready to enqueue for an audio output device.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineAudioBlock {
    pub timeline_frame: i32,
    pub start_sample: i64,
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    pub meters: Vec<StereoTrackMeter>,
    pub errors: Vec<String>,
}

/// Errors that prevent the whole timeline frame from being planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineAudioError {
    InvalidTiming,
}

/// Upper bound for the number of source sample frames one source may decode for a
/// single output block. At 48 kHz stereo this is about 22 seconds, 8 MiB of PCM.
///
/// A nested scene multiplies its speed into the rate of every enclosing scene and
/// clip, so a deep or hand-edited scene graph can ask for an unbounded decode. The
/// workspace builds with `panic = "abort"`, so such an allocation request would end
/// the process instead of dropping one frame. The bound still leaves room for real
/// documents: the editor caps clip speed at 400 % and scene speed at 4x per level,
/// so a preview block only reaches it beyond roughly 1,000x.
const MAX_DECODED_SOURCE_FRAMES: usize = 1_048_576;

/// Source rates this close to unity are treated as unity, skipping resampling.
/// The tolerance only absorbs floating-point noise from composing scene rates; the
/// smallest speed the editor offers (1 %) is orders of magnitude larger.
const UNITY_SOURCE_RATE_EPSILON: f64 = 1.0e-9;

struct DecodedTrack {
    plan: AudioLayerPlan,
    samples: Vec<f32>,
}

/// Reuses per-source FFmpeg decoders while producing independently owned mix blocks.
#[derive(Default)]
pub struct TimelineAudioMixer {
    decoders: HashMap<PathBuf, AudioDecoder>,
    plugin_chains: HashMap<(Option<u64>, i32), AudioPluginChain>,
    noted_rate_limits: HashSet<(Option<u64>, i32)>,
}

impl TimelineAudioMixer {
    pub fn mix_frame_with_sample_count_and_plugin_block_size(
        &mut self,
        timeline_frame: i32,
        fps: f64,
        sample_rate: u32,
        output_frames: usize,
        max_plugin_block_size: usize,
        sources: &[TimelineAudioSource],
    ) -> Result<TimelineAudioBlock, TimelineAudioError> {
        self.mix_frame_with_timeline_rate(
            timeline_frame,
            fps,
            sample_rate,
            output_frames,
            max_plugin_block_size,
            1.0,
            sources,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mix_frame_with_timeline_rate(
        &mut self,
        timeline_frame: i32,
        fps: f64,
        sample_rate: u32,
        output_frames: usize,
        max_plugin_block_size: usize,
        timeline_rate: f64,
        sources: &[TimelineAudioSource],
    ) -> Result<TimelineAudioBlock, TimelineAudioError> {
        let Some(start_sample) = cumulative_samples(timeline_frame, fps, sample_rate) else {
            return Err(TimelineAudioError::InvalidTiming);
        };
        if !timeline_rate.is_finite() || timeline_rate <= 0.0 {
            return Err(TimelineAudioError::InvalidTiming);
        }
        self.mix_planned_frame(
            timeline_frame,
            fps,
            sample_rate,
            start_sample,
            output_frames,
            max_plugin_block_size,
            timeline_rate,
            sources,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn mix_planned_frame(
        &mut self,
        timeline_frame: i32,
        fps: f64,
        sample_rate: u32,
        start_sample: i64,
        output_frames: usize,
        max_plugin_block_size: usize,
        timeline_rate: f64,
        sources: &[TimelineAudioSource],
    ) -> Result<TimelineAudioBlock, TimelineAudioError> {
        let mut boundaries = vec![0, output_frames];
        for source in sources {
            if let Some(timing) = source.plan.nested_timing {
                for fraction in [timing.start_fraction, timing.end_fraction] {
                    if fraction.is_finite() {
                        boundaries.push(
                            (fraction.clamp(0.0, 1.0) * output_frames as f64).round() as usize
                        );
                    }
                }
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let mut samples = Vec::with_capacity(output_frames.saturating_mul(2));
        let mut meters = Vec::new();
        let mut errors = Vec::new();
        for pair in boundaries.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            if start == end {
                continue;
            }
            let interval_sources = sources
                .iter()
                .filter_map(|source| {
                    let Some(timing) = source.plan.nested_timing else {
                        let mut source = source.clone();
                        let offset = start as f64 / output_frames as f64 / fps;
                        source.plan.source_start_time +=
                            (offset * f64::from(source.plan.playback_speed)) as f32;
                        source.plan.direct_time += offset as f32;
                        return Some(source);
                    };
                    let first = (timing.start_fraction.clamp(0.0, 1.0) * output_frames as f64)
                        .round() as usize;
                    let last = (timing.end_fraction.clamp(0.0, 1.0) * output_frames as f64).round()
                        as usize;
                    if first > start || last < end {
                        return None;
                    }
                    let mut source = source.clone();
                    if let Some(timing) = &mut source.plan.nested_timing {
                        let offset =
                            (start as f64 / output_frames as f64 - timing.start_fraction).max(0.0);
                        timing.frame += offset * timing.rate * timing.fps / fps;
                    }
                    Some(source)
                })
                .collect::<Vec<_>>();
            let mixed = self.mix_segment(
                timeline_frame,
                fps,
                sample_rate,
                start_sample.saturating_add(start as i64),
                end - start,
                max_plugin_block_size,
                timeline_rate,
                &interval_sources,
            )?;
            samples.extend(mixed.samples);
            for meter in mixed.meters {
                let weight = (end - start) as f32 / output_frames as f32;
                if let Some(existing) = meters
                    .iter_mut()
                    .find(|existing: &&mut StereoTrackMeter| existing.clip_id == meter.clip_id)
                {
                    existing.mixed |= meter.mixed;
                    existing.peak_left = existing.peak_left.max(meter.peak_left);
                    existing.peak_right = existing.peak_right.max(meter.peak_right);
                    existing.rms_left += meter.rms_left * meter.rms_left * weight;
                    existing.rms_right += meter.rms_right * meter.rms_right * weight;
                } else {
                    meters.push(StereoTrackMeter {
                        rms_left: meter.rms_left * meter.rms_left * weight,
                        rms_right: meter.rms_right * meter.rms_right * weight,
                        ..meter
                    });
                }
            }
            errors.extend(mixed.errors);
        }
        for meter in &mut meters {
            meter.rms_left = meter.rms_left.sqrt();
            meter.rms_right = meter.rms_right.sqrt();
        }
        self.plugin_chains.retain(|key, _| {
            sources.iter().any(|source| {
                (
                    source.plan.nested_timing.map(|timing| timing.instance_key),
                    source.plan.clip_id,
                ) == *key
                    && !source.plan.plugins.is_empty()
            })
        });
        self.noted_rate_limits.retain(|key| {
            sources.iter().any(|source| {
                (
                    source.plan.nested_timing.map(|timing| timing.instance_key),
                    source.plan.clip_id,
                ) == *key
            })
        });
        Ok(TimelineAudioBlock {
            timeline_frame,
            start_sample,
            sample_rate,
            samples,
            meters,
            errors,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn mix_segment(
        &mut self,
        timeline_frame: i32,
        fps: f64,
        sample_rate: u32,
        start_sample: i64,
        output_frames: usize,
        max_plugin_block_size: usize,
        timeline_rate: f64,
        sources: &[TimelineAudioSource],
    ) -> Result<TimelineAudioBlock, TimelineAudioError> {
        let mut decoded_tracks = Vec::with_capacity(sources.len());
        let mut errors = Vec::new();
        for source in sources {
            let key = (
                source.plan.nested_timing.map(|timing| timing.instance_key),
                source.plan.clip_id,
            );
            let source_rate = playback_source_rate(&source.plan, timeline_rate);
            let (source_rate, limited) = bounded_source_rate(source_rate, output_frames);
            if limited {
                // A malformed document would otherwise request a multi-gigabyte
                // decode for one frame; say so once instead of every frame.
                if self.noted_rate_limits.insert(key) {
                    errors.push(format!(
                        "clip #{} ({}): playback rate is out of range; audio is limited to {source_rate:.2}x",
                        source.plan.clip_id,
                        source.path.display()
                    ));
                }
            }
            let resample = (source_rate - 1.0).abs() > UNITY_SOURCE_RATE_EPSILON;
            let source_frames = if resample {
                let frames = ((output_frames as f64 * source_rate).ceil() as usize)
                    .saturating_add(2)
                    .max(1);
                debug_assert!(frames <= MAX_DECODED_SOURCE_FRAMES);
                frames.min(MAX_DECODED_SOURCE_FRAMES)
            } else {
                output_frames
            };
            let source_time = source_time_seconds(&source.plan, timeline_frame, fps);
            let decoded = self
                .decoder(&source.path, sample_rate)
                .and_then(|decoder| decoder.decode_range(source_time, source_frames));
            let mut samples = match decoded {
                Ok(decoded) if !resample => {
                    decoded.samples[..output_frames.saturating_mul(2)].to_vec()
                }
                Ok(decoded) => resample_stereo(&decoded.samples, output_frames, source_rate)
                    .unwrap_or_else(|error| {
                        errors.push(format!(
                            "clip #{} ({}): audio resample failed: {error:?}",
                            source.plan.clip_id,
                            source.path.display()
                        ));
                        vec![0.0; output_frames.saturating_mul(2)]
                    }),
                Err(error) => {
                    errors.push(format!(
                        "clip #{} ({}): {error}",
                        source.plan.clip_id,
                        source.path.display()
                    ));
                    vec![0.0; output_frames.saturating_mul(2)]
                }
            };
            let plugin_key = (
                source.plan.nested_timing.map(|timing| timing.instance_key),
                source.plan.clip_id,
            );
            if !source.plan.plugins.is_empty() {
                self.plugin_chains.entry(plugin_key).or_default().process(
                    &source.plan.plugins,
                    AudioPluginProcessContext {
                        start_sample,
                        sample_rate,
                        max_block_size: max_plugin_block_size,
                    },
                    &mut samples,
                    &mut errors,
                );
            } else {
                self.plugin_chains.remove(&plugin_key);
            }
            decoded_tracks.push(DecodedTrack {
                plan: source.plan.clone(),
                samples,
            });
        }

        let tracks = decoded_tracks
            .iter()
            .map(|track| StereoMixTrack {
                clip_id: track.plan.clip_id,
                samples: &track.samples,
                parameters: mix_parameters(&track.plan, timeline_frame, fps),
                mute: track.plan.mute,
                solo: track.plan.solo,
            })
            .collect::<Vec<_>>();
        let mixed = mix_stereo_tracks(&tracks, output_frames)
            .expect("timeline mixer constructs only interleaved stereo buffers");
        Ok(TimelineAudioBlock {
            timeline_frame,
            start_sample,
            sample_rate,
            samples: mixed.samples,
            meters: mixed.meters,
            errors,
        })
    }

    fn decoder(
        &mut self,
        path: &Path,
        sample_rate: u32,
    ) -> Result<&mut AudioDecoder, freya_nle_media::MediaError> {
        let replace = self
            .decoders
            .get(path)
            .is_some_and(|decoder| decoder.sample_rate() != sample_rate);
        if replace {
            self.decoders.remove(path);
        }
        if !self.decoders.contains_key(path) {
            self.decoders
                .insert(path.to_path_buf(), AudioDecoder::open(path, sample_rate)?);
        }
        Ok(self
            .decoders
            .get_mut(path)
            .expect("audio decoder was inserted above"))
    }
}

/// Clamps a composed source rate so one output frame cannot request an unbounded
/// decode. Returns the effective rate and whether it had to be limited.
///
/// A zero rate is a freeze frame and stays zero; negative and non-finite rates are
/// unusable and fall back to unity so an affected clip still renders.
fn bounded_source_rate(source_rate: f64, output_frames: usize) -> (f64, bool) {
    if source_rate == 0.0 {
        return (0.0, false);
    }
    if !source_rate.is_finite() || source_rate < 0.0 {
        return (1.0, true);
    }
    if output_frames == 0 {
        return (source_rate, false);
    }
    // The decode request adds two guard frames and rounds up, so leave room for both
    // or the bounded rate could still ask for slightly more than the limit.
    let ceiling = (MAX_DECODED_SOURCE_FRAMES.saturating_sub(4)) as f64 / output_frames as f64;
    if source_rate > ceiling {
        (ceiling, true)
    } else {
        (source_rate, false)
    }
}

fn playback_source_rate(plan: &AudioLayerPlan, timeline_rate: f64) -> f64 {
    let nested_rate = plan.nested_timing.map_or(1.0, |timing| timing.rate);
    if plan.direct_mode {
        timeline_rate * nested_rate
    } else {
        f64::from(plan.playback_speed) * timeline_rate * nested_rate
    }
}

fn cumulative_samples(frame: i32, fps: f64, sample_rate: u32) -> Option<i64> {
    let samples = f64::from(frame) * f64::from(sample_rate) / fps;
    if !samples.is_finite() || samples < 0.0 || samples > i64::MAX as f64 {
        return None;
    }
    Some(samples.floor() as i64)
}

fn source_time_seconds(plan: &AudioLayerPlan, timeline_frame: i32, fps: f64) -> f64 {
    if plan.direct_mode {
        let fractional_time = plan.nested_timing.map_or(0.0, |timing| {
            (timing.frame - timing.frame.floor()) / timing.fps
        });
        return f64::from(plan.direct_time) + fractional_time;
    }
    let (frame, fps) = plan
        .nested_timing
        .map_or((f64::from(timeline_frame), fps), |timing| {
            (timing.frame, timing.fps)
        });
    let relative_frames = (frame - f64::from(plan.start_frame)).max(0.0);
    f64::from(plan.source_start_time) + relative_frames / fps * f64::from(plan.playback_speed)
}

fn mix_parameters(plan: &AudioLayerPlan, timeline_frame: i32, fps: f64) -> StereoMixParameters {
    let (frame, fps) = plan
        .nested_timing
        .map_or((f64::from(timeline_frame), fps), |timing| {
            (timing.frame, timing.fps)
        });
    StereoMixParameters {
        relative_time: (frame - f64::from(plan.start_frame)).max(0.0) / fps,
        duration: f64::from(plan.duration_frames.max(0)) / fps,
        fade_in_seconds: plan.fade_in_seconds,
        fade_out_seconds: plan.fade_out_seconds,
        volume: plan.volume,
        master_volume: plan.master_volume,
        pan: plan.pan,
        limiter: plan.limiter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::DEFAULT_PLUGIN_BLOCK_SIZE;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn plan(clip_id: i32) -> AudioLayerPlan {
        AudioLayerPlan {
            clip_id,
            nested_timing: None,
            source_path: Some("missing.wav".to_owned()),
            start_frame: 0,
            duration_frames: 120,
            source_start_time: 1.0,
            playback_speed: 2.0,
            direct_time: 3.0,
            volume: 1.0,
            master_volume: 1.0,
            pan: 0.0,
            fade_in_seconds: 0.0,
            fade_out_seconds: 0.0,
            mute: false,
            solo: false,
            limiter: true,
            direct_mode: false,
            plugins: Vec::new(),
        }
    }

    #[test]
    fn cumulative_frame_ranges_do_not_drift_at_fractional_fps() {
        let mut total = 0_i64;
        for frame in 0..30_000 {
            let plan = freya_nle_core::api::plan_export_audio_frame(frame, 48_000, 30_000, 1_001)
                .expect("timing is valid");
            total += i64::from(plan.samples_for_frame);
        }
        assert_eq!(total, 48_048_000);
    }

    #[test]
    fn preview_timeline_rate_changes_resampling_without_moving_direct_time() {
        let normal = plan(1);
        assert_eq!(playback_source_rate(&normal, 2.0), 4.0);
        let mut direct = normal;
        direct.direct_mode = true;
        assert_eq!(playback_source_rate(&direct, 2.0), 2.0);
        assert_eq!(source_time_seconds(&direct, 30, 60.0), 3.0);
    }

    #[test]
    fn nested_audio_uses_child_timebase_and_composed_rate() {
        let mut plan = plan(1);
        plan.start_frame = 15;
        plan.duration_frames = 60;
        plan.nested_timing = Some(freya_nle_core::api::NestedAudioTiming {
            instance_key: 42,
            frame: 37.5,
            fps: 30.0,
            rate: 1.5,
            start_fraction: 0.0,
            end_fraction: 1.0,
        });
        assert_eq!(source_time_seconds(&plan, 120, 60.0), 2.5);
        assert_eq!(playback_source_rate(&plan, 2.0), 6.0);
        let parameters = mix_parameters(&plan, 120, 60.0);
        assert_eq!(parameters.relative_time, 0.75);
        assert_eq!(parameters.duration, 2.0);
        plan.direct_mode = true;
        assert!((source_time_seconds(&plan, 120, 60.0) - (3.0 + 0.5 / 30.0)).abs() < 1e-10);
        assert_eq!(playback_source_rate(&plan, 2.0), 3.0);
    }

    #[test]
    fn export_sample_count_can_start_at_an_arbitrary_timeline_frame() {
        let fps = 60_000.0 / 1_001.0;
        let mut mixer = TimelineAudioMixer::default();
        let preview_frames = 801; // Preview requests a frame-aligned block at 60000/1001 fps.
        let preview = mixer
            .mix_frame_with_sample_count_and_plugin_block_size(
                1,
                fps,
                48_000,
                preview_frames,
                DEFAULT_PLUGIN_BLOCK_SIZE,
                &[],
            )
            .expect("timeline audio frame mixes");
        let export = mixer
            .mix_frame_with_sample_count_and_plugin_block_size(
                1,
                fps,
                48_000,
                800,
                DEFAULT_PLUGIN_BLOCK_SIZE,
                &[],
            )
            .expect("custom export audio frame mixes");

        assert_eq!(preview.samples.len(), 801 * 2);
        assert_eq!(export.samples.len(), 800 * 2);
        assert_eq!(export.timeline_frame, 1);
        assert_eq!(export.start_sample, 800);
    }

    #[test]
    fn source_time_uses_speed_or_direct_time() {
        let mut plan = plan(1);
        assert_eq!(source_time_seconds(&plan, 30, 60.0), 2.0);
        plan.direct_mode = true;
        assert_eq!(source_time_seconds(&plan, 30, 60.0), 3.0);
    }

    #[test]
    fn near_unity_speeds_still_resample() {
        // AviUtl's speed control is a percentage, so 101 % arrives as 1.01 and lands
        // within a hair of unity. Treating it as unity would silently ignore the
        // speed, so even a 1 % change must take the resampling path.
        let mut plan_a = plan(1);
        plan_a.playback_speed = 1.01;
        let rate = playback_source_rate(&plan_a, 1.0);
        assert!(
            (rate - 1.0).abs() > UNITY_SOURCE_RATE_EPSILON,
            "a 1 % speed change must resample, got rate {rate}"
        );
        // Only floating-point noise from composing scene rates skips resampling.
        let noisy = 1.0 + UNITY_SOURCE_RATE_EPSILON / 4.0;
        assert!((noisy - 1.0).abs() <= UNITY_SOURCE_RATE_EPSILON);

        let mut plan_b = plan(2);
        plan_b.playback_speed = 0.99;
        assert!((playback_source_rate(&plan_b, 1.0) - 1.0).abs() > UNITY_SOURCE_RATE_EPSILON);
    }

    #[test]
    fn extreme_composed_rates_are_bounded_before_decoding() {
        // Three nested levels at the editor's maximum scene speed, the maximum clip
        // speed, and the maximum preview rate compose to a real but large rate.
        let mut plan_a = plan(1);
        plan_a.playback_speed = 4.0;
        plan_a.nested_timing = Some(freya_nle_core::api::NestedAudioTiming {
            instance_key: 3,
            frame: 0.0,
            fps: 60.0,
            rate: 64.0,
            start_fraction: 0.0,
            end_fraction: 1.0,
        });
        let rate = playback_source_rate(&plan_a, 4.0);
        assert!((rate - 1_024.0).abs() < 0.01, "composed rate: {rate}");
        // A minute-long export block is where the composed rate stops being safe.
        let output_frames = 60 * 48_000;
        let (bounded, limited) = bounded_source_rate(rate, output_frames);
        assert!(limited, "1,024x over a minute must be bounded");
        assert!(bounded * output_frames as f64 <= MAX_DECODED_SOURCE_FRAMES as f64);

        // A hand-edited document can compose to infinity, NaN, or a negative speed.
        for impossible in [f64::INFINITY, f64::NAN, -2.0] {
            let (bounded, limited) = bounded_source_rate(impossible, 800);
            assert!(limited, "{impossible} must be reported as limited");
            assert_eq!(bounded, 1.0, "{impossible} must fall back to unity");
        }

        // A freeze frame stays silent instead of being reported as out of range.
        assert_eq!(bounded_source_rate(0.0, 800), (0.0, false));
    }

    #[test]
    fn ordinary_rates_are_not_reported_as_limited() {
        for rate in [0.01, 0.5, 1.0, 1.01, 4.0, 64.0] {
            let (bounded, limited) = bounded_source_rate(rate, 800);
            assert_eq!(bounded, rate);
            assert!(!limited, "{rate} must not be limited");
        }
    }

    /// Writes a mono 16-bit ramp over the first 1,000 samples, then silence, so a
    /// speed change moves the value read at a fixed output position.
    fn write_ramp_wav(path: &Path) -> u32 {
        use std::io::Write;
        const RAMP: u32 = 1_000;
        let sample_count = 48_000_u32;
        let bytes = sample_count * 2;
        let mut file = std::fs::File::create(path).expect("ramp fixture creates");
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + bytes).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&48_000_u32.to_le_bytes()).unwrap();
        file.write_all(&96_000_u32.to_le_bytes()).unwrap();
        file.write_all(&2_u16.to_le_bytes()).unwrap();
        file.write_all(&16_u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&bytes.to_le_bytes()).unwrap();
        for index in 0..sample_count {
            // A 1 in 1,000 rise keeps neighbouring samples distinct, so a 1 % speed
            // change visibly shifts where the output lands on the ramp.
            let value = if index < RAMP {
                (f64::from(index) / f64::from(RAMP) * 24_000.0) as i16
            } else {
                0
            };
            file.write_all(&value.to_le_bytes()).unwrap();
        }
        sample_count
    }

    #[test]
    fn a_one_percent_speed_change_moves_the_decoded_samples() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "freya-near-unity-{}-{nonce}.wav",
            std::process::id()
        ));
        write_ramp_wav(&path);

        let mix = |speed: f32| {
            let mut layer = plan(11);
            layer.source_start_time = 0.0;
            layer.playback_speed = speed;
            let source = TimelineAudioSource {
                path: path.clone(),
                plan: layer,
            };
            TimelineAudioMixer::default()
                .mix_frame_with_sample_count_and_plugin_block_size(
                    0,
                    60.0,
                    48_000,
                    800,
                    DEFAULT_PLUGIN_BLOCK_SIZE,
                    &[source],
                )
                .expect("a valid frame mixes")
        };

        let nominal = mix(1.0);
        let faster = mix(1.01);
        let slower = mix(0.99);
        assert!(nominal.errors.is_empty(), "{:?}", nominal.errors);
        assert!(faster.errors.is_empty(), "{:?}", faster.errors);
        assert!(slower.errors.is_empty(), "{:?}", slower.errors);

        // Skipping resampling near unity would make all three mixes identical, which
        // is what silently ignored AviUtl's 99 % and 101 % speeds. The ramp rises one
        // step per source sample, so a 1 % speed change over 800 frames moves the
        // landing point by eight steps.
        let sample = |block: &TimelineAudioBlock| block.samples[block.samples.len() - 2];
        let last_nominal = sample(&nominal);
        let last_faster = sample(&faster);
        let last_slower = sample(&slower);
        assert!(
            (last_faster - last_nominal).abs() > 1.0e-4,
            "101 % must advance further through the source: {last_faster} vs {last_nominal}"
        );
        assert!(
            (last_slower - last_nominal).abs() > 1.0e-4,
            "99 % must advance less through the source: {last_slower} vs {last_nominal}"
        );
        assert!(
            last_faster > last_nominal && last_nominal > last_slower,
            "a ramp must stay monotonic in speed: {last_faster} > {last_nominal} > {last_slower}"
        );

        std::fs::remove_file(&path).expect("ramp fixture removes");
    }

    #[test]
    fn a_limited_rate_still_mixes_and_reports_once() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "freya-limited-rate-{}-{nonce}.wav",
            std::process::id()
        ));
        write_ramp_wav(&path);

        let mut layer = plan(12);
        layer.source_start_time = 0.0;
        // A hand-edited document can nest scenes until the composed rate is absurd.
        layer.nested_timing = Some(freya_nle_core::api::NestedAudioTiming {
            instance_key: 5,
            frame: 0.0,
            fps: 60.0,
            rate: 1.0e9,
            start_fraction: 0.0,
            end_fraction: 1.0,
        });
        let source = TimelineAudioSource {
            path: path.clone(),
            plan: layer,
        };

        let mut mixer = TimelineAudioMixer::default();
        let first = mixer
            .mix_frame_with_sample_count_and_plugin_block_size(
                0,
                60.0,
                48_000,
                800,
                DEFAULT_PLUGIN_BLOCK_SIZE,
                std::slice::from_ref(&source),
            )
            .expect("the frame still mixes at a bounded rate");
        assert_eq!(first.samples.len(), 1_600);
        assert!(
            first
                .errors
                .iter()
                .any(|error| error.contains("out of range")),
            "a limited rate must be reported: {:?}",
            first.errors
        );

        // The diagnostic is a one-off; repeating it every frame would flood the log.
        let second = mixer
            .mix_frame_with_sample_count_and_plugin_block_size(
                1,
                60.0,
                48_000,
                800,
                DEFAULT_PLUGIN_BLOCK_SIZE,
                &[source],
            )
            .expect("the next frame still mixes");
        assert_eq!(second.samples.len(), 1_600);
        assert!(
            second.errors.is_empty(),
            "the rate warning must not repeat: {:?}",
            second.errors
        );

        // Windows keeps the decoder's handle on the file, and refuses to delete a
        // file that is still open, so the mixer has to go first.
        drop(mixer);
        std::fs::remove_file(&path).expect("ramp fixture removes");
    }

    #[test]
    fn solo_only_silences_other_tracks_during_its_segment() {
        use std::io::Write;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("freya-segment-{}-{nonce}.wav", std::process::id()));
        let mut file = std::fs::File::create(&path).unwrap();
        let sample_count = 4800_u32;
        let bytes = sample_count * 2;
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + bytes).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&48000_u32.to_le_bytes()).unwrap();
        file.write_all(&96000_u32.to_le_bytes()).unwrap();
        file.write_all(&2_u16.to_le_bytes()).unwrap();
        file.write_all(&16_u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&bytes.to_le_bytes()).unwrap();
        for _ in 0..sample_count {
            file.write_all(&8192_i16.to_le_bytes()).unwrap();
        }
        drop(file);
        let mut base = plan(1);
        base.source_start_time = 0.0;
        base.playback_speed = 1.0;
        let mut solo = base.clone();
        solo.clip_id = 2;
        solo.solo = true;
        solo.volume = 0.5;
        solo.nested_timing = Some(freya_nle_core::api::NestedAudioTiming {
            instance_key: 7,
            frame: 1.0,
            fps: 120.0,
            rate: 1.0,
            start_fraction: 0.5,
            end_fraction: 1.0,
        });
        let sources = [
            TimelineAudioSource {
                path: path.clone(),
                plan: base,
            },
            TimelineAudioSource {
                path: path.clone(),
                plan: solo,
            },
        ];
        let block = TimelineAudioMixer::default()
            .mix_frame_with_sample_count_and_plugin_block_size(
                0,
                60.0,
                48000,
                800,
                DEFAULT_PLUGIN_BLOCK_SIZE,
                &sources,
            )
            .unwrap();
        assert!(block.errors.is_empty(), "{:?}", block.errors);
        let first = block.samples[200];
        let second = block.samples[1200];
        assert!(first > 0.1, "first interval: {first}");
        assert!(
            (second / first - 0.5).abs() < 0.02,
            "first: {first}, second: {second}"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_missing_track_is_silent_without_aborting_the_frame() {
        let source = TimelineAudioSource {
            path: PathBuf::from("this-audio-must-not-exist.wav"),
            plan: plan(7),
        };
        let block = TimelineAudioMixer::default()
            .mix_frame_with_sample_count_and_plugin_block_size(
                0,
                60.0,
                48_000,
                800,
                DEFAULT_PLUGIN_BLOCK_SIZE,
                &[source],
            )
            .expect("valid frame timing still mixes");
        assert_eq!(block.samples.len(), 1_600);
        assert!(block.samples.iter().all(|sample| *sample == 0.0));
        assert_eq!(block.errors.len(), 1);
        assert!(block.meters[0].mixed);
    }

    #[test]
    #[ignore = "requires an ffmpeg binary on PATH"]
    fn decodes_and_mixes_consecutive_real_timeline_frames() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            panic!(
                "ffmpeg is required for this test; run with -- --ignored on machines with ffmpeg"
            );
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "freya-nle-timeline-audio-{}-{nonce}.wav",
            std::process::id()
        ));
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:sample_rate=48000:duration=0.2",
                "-c:a",
                "pcm_s16le",
                "-y",
            ])
            .arg(&path)
            .status()
            .expect("ffmpeg process starts");
        assert!(status.success());

        let mut audio_plan = plan(8);
        audio_plan.source_path = Some(path.to_string_lossy().into_owned());
        audio_plan.source_start_time = 0.0;
        audio_plan.playback_speed = 1.0;
        let source = TimelineAudioSource {
            path: path.clone(),
            plan: audio_plan,
        };
        let mut mixer = TimelineAudioMixer::default();
        let first = mixer
            .mix_frame_with_sample_count_and_plugin_block_size(
                0,
                60.0,
                48_000,
                800,
                DEFAULT_PLUGIN_BLOCK_SIZE,
                std::slice::from_ref(&source),
            )
            .expect("first timeline frame mixes");
        let second = mixer
            .mix_frame_with_sample_count_and_plugin_block_size(
                1,
                60.0,
                48_000,
                800,
                DEFAULT_PLUGIN_BLOCK_SIZE,
                &[source],
            )
            .expect("second timeline frame mixes");
        assert_eq!(first.samples.len(), 1_600);
        assert_eq!(second.start_sample, 800);
        assert!(first.errors.is_empty());
        assert!(second.errors.is_empty());
        assert!(first.samples.iter().any(|sample| sample.abs() > 0.01));
        assert!(second.samples.iter().any(|sample| sample.abs() > 0.01));

        drop(mixer);
        std::fs::remove_file(path).expect("generated audio removes");
    }
}

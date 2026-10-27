use crate::abi::{AviQtlAudioMeter, AviQtlAudioMixParameters};

/// Safe Rust parameters for mixing one stereo track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StereoMixParameters {
    pub relative_time: f64,
    pub duration: f64,
    pub fade_in_seconds: f32,
    pub fade_out_seconds: f32,
    pub volume: f32,
    pub master_volume: f32,
    pub pan: f32,
    pub limiter: bool,
}

/// Borrowed stereo samples and timeline selection state for one track.
#[derive(Debug, Clone, Copy)]
pub struct StereoMixTrack<'a> {
    pub clip_id: i32,
    pub samples: &'a [f32],
    pub parameters: StereoMixParameters,
    pub mute: bool,
    pub solo: bool,
}

/// Per-track metering produced by [`mix_stereo_tracks`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StereoTrackMeter {
    pub clip_id: i32,
    pub mixed: bool,
    pub peak_left: f32,
    pub peak_right: f32,
    pub rms_left: f32,
    pub rms_right: f32,
}

/// Owned master buffer and per-track meters for one mix operation.
#[derive(Debug, Clone, PartialEq)]
pub struct StereoMix {
    pub samples: Vec<f32>,
    pub meters: Vec<StereoTrackMeter>,
}

/// Validation errors for the safe Rust audio processing surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProcessError {
    NonStereoBuffer,
    InvalidSourceRate,
    MissingSourceSamples,
}

/// Linearly resamples interleaved stereo samples for timeline playback speed changes.
pub fn resample_stereo(
    input: &[f32],
    output_frames: usize,
    source_rate: f64,
) -> Result<Vec<f32>, AudioProcessError> {
    if !input.len().is_multiple_of(2) {
        return Err(AudioProcessError::NonStereoBuffer);
    }
    if !source_rate.is_finite() || source_rate < 0.0 {
        return Err(AudioProcessError::InvalidSourceRate);
    }
    if output_frames != 0 && input.is_empty() {
        return Err(AudioProcessError::MissingSourceSamples);
    }
    let mut output = vec![0.0; output_frames.saturating_mul(2)];
    resample_stereo_linear(input, &mut output, source_rate);
    Ok(output)
}

/// Mixes interleaved stereo tracks using AviQtl's solo, mute, gain, pan, fade, and limiter rules.
pub fn mix_stereo_tracks(
    tracks: &[StereoMixTrack<'_>],
    output_frames: usize,
) -> Result<StereoMix, AudioProcessError> {
    if tracks
        .iter()
        .any(|track| !track.samples.len().is_multiple_of(2))
    {
        return Err(AudioProcessError::NonStereoBuffer);
    }
    let has_solo = tracks.iter().any(|track| track.solo && !track.mute);
    let mut master = vec![0.0; output_frames.saturating_mul(2)];
    let mut meters = Vec::with_capacity(tracks.len());
    for track in tracks {
        let mut result = StereoTrackMeter {
            clip_id: track.clip_id,
            ..StereoTrackMeter::default()
        };
        if !track.mute && (!has_solo || track.solo) {
            let meter = mix_stereo(track.samples, &mut master, track.parameters.into());
            result.mixed = true;
            result.peak_left = meter.peak_left;
            result.peak_right = meter.peak_right;
            result.rms_left = meter.rms_left;
            result.rms_right = meter.rms_right;
        }
        meters.push(result);
    }
    Ok(StereoMix {
        samples: master,
        meters,
    })
}

impl From<StereoMixParameters> for AviQtlAudioMixParameters {
    fn from(parameters: StereoMixParameters) -> Self {
        Self {
            relative_time: parameters.relative_time,
            duration: parameters.duration,
            fade_in_seconds: parameters.fade_in_seconds,
            fade_out_seconds: parameters.fade_out_seconds,
            volume: parameters.volume,
            master_volume: parameters.master_volume,
            pan: parameters.pan,
            limiter: u32::from(parameters.limiter),
        }
    }
}

fn fade_gain(parameters: AviQtlAudioMixParameters) -> f32 {
    let mut gain: f64 = 1.0;
    if parameters.fade_in_seconds > 0.0 {
        gain = gain.min(
            (parameters.relative_time / f64::from(parameters.fade_in_seconds)).clamp(0.0, 1.0),
        );
    }
    if parameters.fade_out_seconds > 0.0 {
        gain = gain.min(
            ((parameters.duration - parameters.relative_time)
                / f64::from(parameters.fade_out_seconds))
            .clamp(0.0, 1.0),
        );
    }
    gain as f32
}

fn resample_stereo_linear(input: &[f32], output: &mut [f32], source_rate: f64) {
    let input_frames = input.len() / 2;
    for (frame, output_frame) in output.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let source_index = frame as f64 * source_rate;
        let first_index = (source_index as usize).min(input_frames - 1);
        let second_index = first_index.saturating_add(1).min(input_frames - 1);
        let fraction = source_index - first_index as f64;

        output_frame[0] = (f64::from(input[first_index * 2]) * (1.0 - fraction)
            + f64::from(input[second_index * 2]) * fraction) as f32;
        output_frame[1] = (f64::from(input[first_index * 2 + 1]) * (1.0 - fraction)
            + f64::from(input[second_index * 2 + 1]) * fraction) as f32;
    }
}

fn mix_stereo(
    clip: &[f32],
    master: &mut [f32],
    parameters: AviQtlAudioMixParameters,
) -> AviQtlAudioMeter {
    let gain = parameters.volume * parameters.master_volume * fade_gain(parameters);
    let left_gain = gain
        * if parameters.pan <= 0.0 {
            1.0
        } else {
            1.0 - parameters.pan
        };
    let right_gain = gain
        * if parameters.pan >= 0.0 {
            1.0
        } else {
            1.0 + parameters.pan
        };

    let mut meter = AviQtlAudioMeter::default();
    let mut square_left = 0.0;
    let mut square_right = 0.0;
    for (clip_frame, master_frame) in clip
        .as_chunks::<2>()
        .0
        .iter()
        .zip(master.as_chunks_mut::<2>().0.iter_mut())
    {
        let mut left = clip_frame[0] * left_gain;
        let mut right = clip_frame[1] * right_gain;
        if parameters.limiter != 0 {
            left = left.clamp(-1.0, 1.0);
            right = right.clamp(-1.0, 1.0);
        }

        meter.peak_left = meter.peak_left.max(left.abs());
        meter.peak_right = meter.peak_right.max(right.abs());
        square_left += f64::from(left) * f64::from(left);
        square_right += f64::from(right) * f64::from(right);
        master_frame[0] += left;
        master_frame[1] += right;
    }

    let master_frames = master.len() / 2;
    if !clip.is_empty() && master_frames != 0 {
        let denominator = master_frames as f64;
        meter.rms_left = (square_left / denominator).sqrt() as f32;
        meter.rms_right = (square_right / denominator).sqrt() as f32;
    }
    meter
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "actual={actual:.9e} expected={expected:.9e}"
        );
    }

    #[test]
    fn safe_resampling_surface_matches_the_legacy_kernel() {
        let input = [0.25, -0.5, 0.75, 0.5, -1.0, 1.0, 0.5, -0.25];
        let output = resample_stereo(&input, 5, 0.75).expect("valid stereo resamples");
        let expected = [
            0.25, -0.5, 0.625, 0.25, -0.125, 0.75, -0.625, 0.6875, 0.5, -0.25,
        ];
        for (actual, expected) in output.into_iter().zip(expected) {
            assert_close(actual, expected);
        }
        assert_eq!(
            resample_stereo(&[0.0], 1, 1.0),
            Err(AudioProcessError::NonStereoBuffer)
        );
    }

    #[test]
    fn safe_batch_mixing_preserves_solo_and_metering_rules() {
        let solo_samples = [0.5_f32, -0.5, 0.25, -0.25];
        let skipped_samples = [1.0_f32, 1.0, 1.0, 1.0];
        let parameters = StereoMixParameters {
            relative_time: 0.0,
            duration: 1.0,
            fade_in_seconds: 0.0,
            fade_out_seconds: 0.0,
            volume: 1.0,
            master_volume: 1.0,
            pan: 0.0,
            limiter: false,
        };
        let mixed = mix_stereo_tracks(
            &[
                StereoMixTrack {
                    clip_id: 10,
                    samples: &solo_samples,
                    parameters,
                    mute: false,
                    solo: true,
                },
                StereoMixTrack {
                    clip_id: 11,
                    samples: &skipped_samples,
                    parameters,
                    mute: false,
                    solo: false,
                },
            ],
            2,
        )
        .expect("valid tracks mix");
        assert_eq!(mixed.samples, solo_samples);
        assert!(mixed.meters[0].mixed);
        assert_close(mixed.meters[0].peak_left, 0.5);
        assert!(!mixed.meters[1].mixed);
    }
}

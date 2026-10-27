use crate::abi::AviQtlEasingParameters;
use crate::{EasingKind, evaluate};

fn qt_hash_word(mut key: usize) -> usize {
    key ^= key >> 32;
    key = key.wrapping_mul(0xd6e8_feb8_6659_fd93);
    key ^= key >> 32;
    key = key.wrapping_mul(0xd6e8_feb8_6659_fd93);
    key ^ (key >> 32)
}

#[cfg(target_pointer_width = "32")]
fn qt_hash_word(mut key: usize) -> usize {
    key ^= key >> 16;
    key = key.wrapping_mul(0x045d_9f3b);
    key ^= key >> 16;
    key = key.wrapping_mul(0x045d_9f3b);
    key ^ (key >> 16)
}

fn qt_hash_i32(value: i32) -> usize {
    qt_hash_word(value as isize as usize)
}

#[cfg(target_pointer_width = "64")]
fn qt_hash_i64(value: i64) -> usize {
    qt_hash_word(value as usize)
}

#[cfg(target_pointer_width = "32")]
fn qt_hash_i64(value: i64) -> usize {
    let bits = value as u64;
    qt_hash_word(((bits as u32) ^ ((bits >> 32) as u32)) as usize)
}

fn scaled_random_value(value: f64) -> Option<i64> {
    let scaled = value * 1000.0;
    if scaled.is_finite() && scaled >= i64::MIN as f64 && scaled < i64::MAX as f64 {
        Some(scaled as i64)
    } else {
        None
    }
}

fn random_interpolation(a: f64, b: f64, f0: i32, f1: i32, step_index: i32) -> f64 {
    let seed = qt_hash_i32(f0)
        ^ qt_hash_i32(f1)
        ^ qt_hash_i32(step_index)
        ^ qt_hash_i64(scaled_random_value(a).expect("validated random value"))
        ^ qt_hash_i64(scaled_random_value(b).expect("validated random value"));
    let fraction = f64::from((seed as u32) % 1_000_000) / 999_999.0;
    a.min(b) + (a.max(b) - a.min(b)) * fraction
}

pub(crate) struct NumericSegment<'a> {
    pub interpolation: &'a str,
    pub first_value: f64,
    pub second_value: f64,
    pub first_frame: i32,
    pub second_frame: i32,
    pub frame: i32,
    pub custom_points: &'a [f64],
    pub amplitude: f64,
    pub period: f64,
    pub step_frames: i32,
}

pub(crate) fn evaluate_numeric_segment(segment: NumericSegment<'_>) -> f64 {
    if segment.first_frame == segment.second_frame {
        return segment.first_value;
    }
    let frame_offset = i64::from(segment.frame) - i64::from(segment.first_frame);
    let frame_delta = i64::from(segment.second_frame) - i64::from(segment.first_frame);
    let t = frame_offset as f64 / frame_delta as f64;
    match segment.interpolation {
        "none" => segment.first_value,
        "random" => {
            if scaled_random_value(segment.first_value).is_none()
                || scaled_random_value(segment.second_value).is_none()
            {
                return segment.first_value;
            }
            let step_frames = i64::from(segment.step_frames.max(1));
            random_interpolation(
                segment.first_value,
                segment.second_value,
                segment.first_frame,
                segment.second_frame,
                (frame_offset / step_frames) as i32,
            )
        }
        "alternate" => {
            let step_frames = i64::from(segment.step_frames.max(1));
            if (frame_offset / step_frames) % 2 == 0 {
                segment.first_value
            } else {
                segment.second_value
            }
        }
        name => {
            let kind = EasingKind::from_name(name).unwrap_or(EasingKind::Linear);
            let eased = evaluate(
                kind,
                t,
                segment.custom_points,
                AviQtlEasingParameters {
                    amplitude: segment.amplitude,
                    period: segment.period,
                },
            );
            segment.first_value + (segment.second_value - segment.first_value) * eased
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment<'a>(
        interpolation: &'a str,
        frame: i32,
        first: (i32, f64),
        second: (i32, f64),
        custom_points: &'a [f64],
    ) -> NumericSegment<'a> {
        NumericSegment {
            interpolation,
            first_value: first.1,
            second_value: second.1,
            first_frame: first.0,
            second_frame: second.0,
            frame,
            custom_points,
            amplitude: 1.0,
            period: 0.3,
            step_frames: 1,
        }
    }

    #[test]
    fn evaluates_linear_none_random_alternate_and_duplicates() {
        assert_eq!(
            evaluate_numeric_segment(segment("linear", 50, (0, 0.0), (100, 100.0), &[])),
            50.0
        );
        assert_eq!(
            evaluate_numeric_segment(segment("none", 50, (0, 10.0), (100, 20.0), &[])),
            10.0
        );
        assert_eq!(
            evaluate_numeric_segment(segment("linear", 50, (50, 10.0), (50, 20.0), &[])),
            10.0
        );
        let value = evaluate_numeric_segment(segment("random", 50, (0, 0.0), (100, 10.0), &[]));
        assert!((0.0..=10.0).contains(&value));
        let alternate = evaluate_numeric_segment(NumericSegment {
            step_frames: 5,
            ..segment("alternate", 50, (0, 10.0), (100, 20.0), &[])
        });
        assert_eq!(alternate, 10.0);
    }

    #[test]
    fn evaluates_easing_and_custom_points() {
        let points = [0.33, 0.0, 0.66, 1.0, 1.0, 1.0];
        let value =
            evaluate_numeric_segment(segment("custom", 50, (0, 0.0), (100, 100.0), &points));
        assert!((value - 50.0).abs() < 1.0);

        let eased =
            evaluate_numeric_segment(segment("ease_out_bounce", 25, (0, 0.0), (100, 100.0), &[]));
        assert!(eased > 0.0 && eased < 100.0);
        // An unknown name falls back to linear.
        assert_eq!(
            evaluate_numeric_segment(segment("unknown", 50, (0, 0.0), (100, 100.0), &[])),
            50.0
        );
    }

    #[test]
    fn easing_names_cover_the_document_interpolations() {
        assert_eq!(
            EasingKind::from_name("ease_out_bounce").map(|kind| kind as u32),
            Some(EasingKind::EaseOutBounce as u32)
        );
        assert_eq!(
            EasingKind::from_name("custom").map(|kind| kind as u32),
            Some(EasingKind::Custom as u32)
        );
        assert!(EasingKind::from_name("none").is_none());
        assert!(EasingKind::from_name("random").is_none());
        assert!(EasingKind::from_name("alternate").is_none());
        assert!(EasingKind::from_name("unknown").is_none());
    }
}

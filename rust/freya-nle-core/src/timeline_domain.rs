use crate::abi::{AviQtlIdAllocation, AviQtlSceneSettings, AviQtlTimelineClipGeometry};
use crate::project::DEFAULT_FPS;
use std::collections::BTreeSet;

pub(crate) fn allocate_id(
    existing_ids: &[i32],
    next_hint: i32,
    minimum_id: i32,
) -> Option<AviQtlIdAllocation> {
    let minimum_id = minimum_id.max(0);
    let mut candidate = next_hint.max(minimum_id);
    while existing_ids.contains(&candidate) {
        candidate = candidate.checked_add(1)?;
    }
    Some(AviQtlIdAllocation {
        allocated_id: candidate,
        next_id: candidate.saturating_add(1),
    })
}

fn rounded_non_negative(value: f64) -> i32 {
    if value.is_finite() {
        value.round().max(0.0).min(f64::from(i32::MAX)) as i32
    } else {
        0
    }
}

pub(crate) fn snap_frame(
    frame: f64,
    ignore_snap: bool,
    settings: AviQtlSceneSettings,
    timeline_scale: f64,
) -> i32 {
    if ignore_snap || settings.enable_snap == 0 {
        return rounded_non_negative(frame);
    }

    let fps = if settings.fps.is_finite() {
        settings.fps.max(1.0)
    } else {
        DEFAULT_FPS
    };
    let scale = if timeline_scale.is_finite() {
        timeline_scale
    } else {
        1.0
    };
    let mut step = 1.0;
    let mut offset = 0.0;
    match settings.grid_mode {
        1 => {
            let bpm = if settings.grid_bpm.is_finite() {
                settings.grid_bpm.max(1.0)
            } else {
                120.0
            };
            let subdivision = if scale > 3.0 {
                4.0
            } else if scale > 1.5 {
                2.0
            } else {
                1.0
            };
            step = (fps / (bpm / 60.0)) / subdivision;
            offset = if settings.grid_offset.is_finite() {
                settings.grid_offset * fps
            } else {
                0.0
            };
        }
        2 => step = f64::from(settings.grid_interval.max(1)),
        _ if scale < 0.5 => step = fps.ceil(),
        _ if scale < 1.5 => step = 10.0,
        _ if scale < 3.0 => step = 5.0,
        _ => {}
    }
    rounded_non_negative(((frame - offset) / step).round() * step + offset)
}

pub(crate) fn timeline_duration(clips: &[AviQtlTimelineClipGeometry]) -> i32 {
    clips
        .iter()
        .filter(|clip| clip.duration_frames > 0)
        .map(|clip| clip.start_frame.saturating_add(clip.duration_frames))
        .max()
        .unwrap_or(0)
        .max(1)
}

fn inverse_permutation(permutation: &[i32]) -> Option<Vec<i32>> {
    let mut inverse = vec![-1_i32; permutation.len()];
    for (new_index, old_index) in permutation.iter().copied().enumerate() {
        let slot = usize::try_from(old_index)
            .ok()
            .and_then(|index| inverse.get_mut(index))?;
        if *slot >= 0 {
            return None;
        }
        *slot = i32::try_from(new_index).ok()?;
    }
    inverse.iter().all(|index| *index >= 0).then_some(inverse)
}

pub(crate) fn plan_index_move(
    length: usize,
    old_index: i32,
    new_index: i32,
    minimum_index: i32,
) -> Option<(Vec<i32>, Vec<i32>)> {
    if length > i32::MAX as usize
        || old_index < minimum_index
        || new_index < minimum_index
        || usize::try_from(old_index).ok()? >= length
        || usize::try_from(new_index).ok()? >= length
    {
        return None;
    }
    let mut permutation: Vec<_> = (0..length as i32).collect();
    let moved = permutation.remove(old_index as usize);
    permutation.insert(new_index as usize, moved);
    let inverse = inverse_permutation(&permutation)?;
    Some((permutation, inverse))
}

fn normalize_removal_indices(length: usize, indices: &[i32], minimum_index: i32) -> Vec<i32> {
    let minimum_index = minimum_index.max(0);
    let mut normalized = BTreeSet::new();
    for index in indices {
        if *index >= minimum_index && usize::try_from(*index).is_ok_and(|index| index < length) {
            normalized.insert(*index);
        }
    }
    normalized.into_iter().rev().collect()
}

pub(crate) fn plan_multi_reorder(
    length: usize,
    indices: &[i32],
    target_index: i32,
    minimum_index: i32,
) -> Option<(Vec<i32>, Vec<i32>, usize)> {
    if length > i32::MAX as usize || target_index < 0 || minimum_index < 0 {
        return None;
    }
    let selected_ascending: Vec<_> = normalize_removal_indices(length, indices, minimum_index)
        .into_iter()
        .rev()
        .collect();
    if selected_ascending.is_empty() {
        return None;
    }
    let selected: BTreeSet<_> = selected_ascending.iter().copied().collect();
    let remaining: Vec<_> = (0..length as i32)
        .filter(|index| !selected.contains(index))
        .collect();
    let count_before = selected_ascending
        .iter()
        .filter(|index| **index < target_index)
        .count() as i32;
    let insert_at =
        (target_index - count_before).clamp(minimum_index, remaining.len() as i32) as usize;
    let mut permutation = Vec::with_capacity(length);
    permutation.extend_from_slice(&remaining[..insert_at]);
    permutation.extend_from_slice(&selected_ascending);
    permutation.extend_from_slice(&remaining[insert_at..]);
    let inverse = inverse_permutation(&permutation)?;
    Some((permutation, inverse, selected_ascending.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_skips_existing_ids_and_respects_minimum() {
        assert_eq!(
            allocate_id(&[1, 2, 4], 1, 1),
            Some(AviQtlIdAllocation {
                allocated_id: 3,
                next_id: 4,
            })
        );
        assert_eq!(allocate_id(&[], -5, 1).unwrap().allocated_id, 1);
    }

    #[test]
    fn grid_snapping_and_duration_rules_match_the_timeline() {
        let settings = AviQtlSceneSettings {
            width: 1920,
            height: 1080,
            fps: 30.0,
            total_frames: 300,
            grid_mode: 2,
            grid_bpm: 120.0,
            grid_offset: 0.0,
            grid_interval: 10,
            grid_subdivision: 4,
            enable_snap: 1,
            magnetic_snap_range: 10,
        };
        assert_eq!(snap_frame(16.0, false, settings, 1.0), 20);
        assert_eq!(snap_frame(16.0, true, settings, 1.0), 16);

        let clips = [
            AviQtlTimelineClipGeometry {
                clip_id: 1,
                layer: 0,
                start_frame: 5,
                duration_frames: 10,
            },
            AviQtlTimelineClipGeometry {
                clip_id: 2,
                layer: 1,
                start_frame: 20,
                duration_frames: 30,
            },
        ];
        assert_eq!(timeline_duration(&clips), 50);
        assert_eq!(timeline_duration(&[]), 1);
    }
}

//! Shared stable-model updates and project/transport projection.

use crate::localization::{localized, localized_effect_metadata, status_line};
use crate::object_settings::finite_f32;
use crate::{
    LayerData, MainWindow, MediaBinEntryData, MediaBinGroupData, MissingMediaData, ProjectTabData,
    SceneTabData, TimelineClipData, TimelineMarkData, TimelineObjectData, TimelineWindow,
};
use freya_nle_app::settings::SettingsStore;
use freya_nle_app::{ApplicationModel, MAX_TIMELINE_LAYERS};
use slint::{Color, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

pub(super) fn setting_string(settings: &SettingsStore, key: &str, fallback: &str) -> String {
    settings
        .value(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

pub(super) fn sync_weak_windows(
    main: &slint::Weak<MainWindow>,
    timeline: &slint::Weak<TimelineWindow>,
    model: &Rc<RefCell<ApplicationModel>>,
) {
    if let (Some(main), Some(timeline)) = (main.upgrade(), timeline.upgrade()) {
        sync_windows(&main, &timeline, &model.borrow());
    }
}

pub(super) fn sync_transport_weak(
    main: &slint::Weak<MainWindow>,
    timeline: &slint::Weak<TimelineWindow>,
    model: &Rc<RefCell<ApplicationModel>>,
) {
    if let (Some(main), Some(timeline)) = (main.upgrade(), timeline.upgrade()) {
        sync_transport(&main, &timeline, &model.borrow());
    }
}

pub(super) fn sync_windows(main: &MainWindow, timeline: &TimelineWindow, model: &ApplicationModel) {
    sync_project_tabs(main, timeline, model);

    let Some(workspace) = model.current_workspace() else {
        update_vec_model(&main.get_missing_media(), Vec::new());
        main.set_missing_media_dialog_visible(false);
        // The timeline window's copy of the menu greys out the same item.
        timeline.set_has_missing_media(false);
        update_vec_model(&timeline.get_scene_tabs(), Vec::new());
        update_vec_model(&timeline.get_clips(), Vec::new());
        update_vec_model(&timeline.get_layers(), Vec::new());
        update_vec_model(&timeline.get_media_entries(), Vec::new());
        update_vec_model(&timeline.get_media_groups(), Vec::new());
        update_vec_model(&timeline.get_objects(), Vec::new());
        // The object-list dock reads this model, so it has to be cleared with
        // `objects` or the closed project's rows stay on screen.
        update_vec_model(&timeline.get_object_rows(), Vec::new());
        // No project means no marks: both menus would otherwise keep the last
        // project's list and offer commands that cannot run.
        sync_marks(main, timeline, model);
        timeline.set_playhead(0);
        timeline.set_duration(0);
        timeline.set_playback_duration(0);
        timeline.set_selected_clip_count(0);
        // The footer's right reading belongs to the selected scene; with no
        // project open it must not keep the last project's name.
        timeline.set_scene_name(SharedString::default());
        timeline.set_scene_resolution(SharedString::default());
        timeline.set_scene_frame_rate(60);
        return;
    };
    let missing_media = workspace.missing_media();
    let missing_media = missing_media
        .into_iter()
        .map(|entry| MissingMediaData {
            clip_id: entry.clip_id,
            scene_id: entry.scene_id,
            layer: entry.layer,
            clip_type: SharedString::from(entry.clip_type),
            path: SharedString::from(entry.path),
            name: SharedString::from(entry.name),
        })
        .collect::<Vec<_>>();
    let has_missing_media = !missing_media.is_empty();
    update_vec_model(&main.get_missing_media(), missing_media);
    timeline.set_has_missing_media(has_missing_media);
    if !has_missing_media {
        main.set_missing_media_dialog_visible(false);
    }
    let scenes = workspace
        .scene_tabs()
        .into_iter()
        .map(|scene| SceneTabData {
            id: scene.id,
            name: SharedString::from(scene.name),
            label: SharedString::from(scene.label),
            selected: scene.selected,
            root: scene.root,
        })
        .collect::<Vec<_>>();
    update_vec_model(&timeline.get_scene_tabs(), scenes);
    // The filter combo lists "All labels" first; a label that vanished (renamed
    // or removed) resets the filter instead of hiding every scene.
    let label_names = workspace
        .scene_labels()
        .into_iter()
        .map(SharedString::from)
        .collect::<Vec<_>>();
    let filter = timeline.get_scene_label_filter();
    if !filter.is_empty()
        && !label_names
            .iter()
            .any(|label| label.as_str() == filter.as_str())
    {
        timeline.set_scene_label_filter(SharedString::default());
    }
    update_vec_model(&timeline.get_scene_label_names(), label_names);
    let clips = workspace
        .timeline_clips()
        .into_iter()
        .map(|clip| {
            let style = timeline_clip_style(&clip.clip_type);
            let has_clip_color = style.is_some();
            let (clip_color, dark_text) = style.unwrap_or_default();
            TimelineClipData {
                id: clip.id,
                label: SharedString::from(localized_effect_metadata(&clip.label).into_owned()),
                clip_type: SharedString::from(clip.clip_type),
                clip_color,
                has_clip_color,
                dark_text,
                waveform: ModelRc::new(VecModel::<f32>::default()),
                start: clip.start,
                duration: clip.duration,
                layer: clip.layer,
                audio: clip.audio,
                locked: clip.locked,
                control_layer_count: clip.control_layer_count,
                clip_by_upper_object: clip.clip_by_upper_object,
                selected: clip.selected,
                primary: clip.primary,
                grouped: clip.grouped,
                midpoints: ModelRc::new(VecModel::from(workspace.clip_midpoints(clip.id))),
            }
        })
        .collect::<Vec<_>>();
    update_vec_model(&timeline.get_clips(), clips);
    let kinds = workspace.object_kinds_at_playhead();
    // The panel stores a kind id; the combo box shows its localized label. A
    // kind that disappears from the frame falls back to "all" so the list never
    // filters itself empty.
    let labels = kinds
        .iter()
        .map(|kind| object_kind_label(kind))
        .collect::<Vec<_>>();
    let mut filter = timeline.get_object_kind_filter_value().to_string();
    if !kinds.iter().any(|kind| kind == &filter) {
        filter.clear();
        timeline.set_object_kind_filter_value(SharedString::default());
    }
    let label = kinds
        .iter()
        .position(|kind| kind == &filter)
        .and_then(|index| labels.get(index).cloned())
        .unwrap_or_default();
    if timeline.get_object_kind_label() != label {
        timeline.set_object_kind_label(SharedString::from(label));
    }
    update_vec_model(
        &timeline.get_object_kind_filters(),
        labels
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    );
    let objects = workspace
        .objects_at_playhead()
        .into_iter()
        .filter(|object| filter.is_empty() || object.clip_type == filter)
        .map(|object| TimelineObjectData {
            clip_id: object.clip_id,
            label: SharedString::from(localized_effect_metadata(&object.label).into_owned()),
            layer: object.layer,
            kind: SharedString::from(object.clip_type),
            locked: object.locked,
            selected: object.selected,
            clip_by_upper_object: object.clip_by_upper_object,
        })
        .collect::<Vec<_>>();
    update_vec_model(&timeline.get_objects(), objects.clone());
    update_vec_model(&timeline.get_object_rows(), objects);
    let selected_layer = workspace.selected_layer();
    let scene = workspace.selected_scene_document();
    let layer_settings = workspace.layer_settings();
    let layers = (0..timeline.get_maximum_layers().clamp(1, MAX_TIMELINE_LAYERS))
        .map(|index| {
            let setting = layer_settings.iter().find(|setting| setting.layer == index);
            LayerData {
                index,
                visible: scene.is_none_or(|scene| !scene.hidden_layers.contains(&index)),
                locked: scene.is_some_and(|scene| scene.locked_layers.contains(&index)),
                selected: index == selected_layer,
                name: SharedString::from(setting.map_or("", |setting| setting.name.as_str())),
                background: setting
                    .and_then(|setting| layer_background_color(&setting.background))
                    .unwrap_or(Color::from_argb_u8(0, 0, 0, 0)),
                has_background: setting.is_some_and(|setting| !setting.background.is_empty()),
            }
        })
        .collect::<Vec<_>>();
    update_vec_model(&timeline.get_layers(), layers);
    sync_media_bin(timeline, workspace);
    sync_transport(main, timeline, model);
}

pub(super) fn sync_media_bin(timeline: &TimelineWindow, workspace: &freya_nle_app::WorkspaceModel) {
    let bin = workspace.media_bin();
    let query = timeline.get_media_filter().trim().to_lowercase();
    let filter = timeline.get_media_group_filter();
    let entries = bin
        .entries()
        .iter()
        .filter(|entry| {
            (filter == -2
                || (filter == -1
                    && bin
                        .bins()
                        .iter()
                        .all(|group| !group.entry_ids.contains(&entry.id)))
                || bin
                    .bins()
                    .iter()
                    .any(|group| group.id == filter && group.entry_ids.contains(&entry.id)))
                && (query.is_empty()
                    || entry.name.to_lowercase().contains(&query)
                    || entry.path.to_lowercase().contains(&query))
        })
        .map(|entry| {
            let group = bin
                .bins()
                .iter()
                .find(|group| group.entry_ids.contains(&entry.id));
            MediaBinEntryData {
                id: entry.id,
                path: SharedString::from(&entry.path),
                name: SharedString::from(&entry.name),
                // A stable key, not a label: the panel translates it, so the kind column reads
                // the same language as the rest of the window.
                kind: SharedString::from(match entry.kind {
                    freya_nle_app::media_import::MediaImportKind::Video => "video",
                    freya_nle_app::media_import::MediaImportKind::Audio => "audio",
                    freya_nle_app::media_import::MediaImportKind::Image => "image",
                }),
                group_id: group.map_or(-1, |group| group.id),
                group_name: SharedString::from(group.map_or("", |group| group.name.as_str())),
                group_index: group
                    .and_then(|group| {
                        bin.bins()
                            .iter()
                            .position(|candidate| candidate.id == group.id)
                    })
                    .map_or(-1, |index| index as i32),
                duration_frames: entry.duration_frames,
                in_frame: entry.in_frame,
                out_frame: entry.out_frame,
                selected: timeline.get_selected_media_id() == entry.id,
            }
        })
        .collect::<Vec<MediaBinEntryData>>();
    // A selection that the filter or a removal hid would leave the Remove button pointed at an
    // entry nobody can see, so it is dropped instead of remembered.
    let selected = timeline.get_selected_media_id();
    if selected >= 0 && !entries.iter().any(|entry| entry.id == selected) {
        timeline.set_selected_media_id(-1);
    }
    update_vec_model(&timeline.get_media_entries(), entries);
    update_vec_model(
        &timeline.get_media_groups(),
        bin.bins()
            .iter()
            .map(|group| MediaBinGroupData {
                id: group.id,
                name: SharedString::from(&group.name),
            })
            .collect(),
    );
    // The filter combo shows two synthetic rows before the real groups. A removed group would
    // otherwise leave the box pointing at a stale index, so reset to "All media" when the saved
    // filter no longer exists.
    let filter_still_exists =
        filter == -2 || filter == -1 || bin.bins().iter().any(|group| group.id == filter);
    if !filter_still_exists {
        timeline.set_media_group_filter(-2);
        timeline.set_selected_media_id(-1);
    }
    // The filter combo shows two synthetic rows before the real groups, so the host owns the
    // whole list: a translated label cannot be produced from Slint's `@tr` for a row whose text
    // comes from Rust, and keeping the two halves in one place is what stops them disagreeing.
    let all_media = localized("All media", "全部媒体", "すべてのメディア");
    let ungrouped = localized("Ungrouped", "未分组", "未分類");
    let names = std::iter::once(all_media.to_owned())
        .chain(std::iter::once(ungrouped.to_owned()))
        .chain(bin.bins().iter().map(|group| group.name.clone()))
        .map(SharedString::from)
        .collect::<Vec<_>>();
    // Replaced wholesale rather than updated in place: a combo box caches the label it shows and
    // does not notice rows added to an existing model, so a fresh model is what makes the filter
    // box show its first entry instead of staying blank.
    timeline.set_media_group_names(ModelRc::from(Rc::new(VecModel::from(names))));
    // The combo's index is derived here rather than kept in the panel: an empty model at startup
    // leaves a widget holding index -1, and only the host knows which index the saved filter has.
    let group_index = if filter == -2 {
        0
    } else if filter == -1 {
        1
    } else {
        bin.bins()
            .iter()
            .position(|group| group.id == filter)
            .map_or(0, |index| index as i32 + 2)
    };
    timeline.set_media_group_index(group_index);
}

/// Clip colour for a type, matching AviUtl2's object groups (style.conf
/// `[Color]`): audio objects are red, control objects (camera, scene
/// reference) are cyan, and media objects keep the theme's video blue.
///
/// The colours live here rather than in `ui/theme.slint` because the
/// projection only has `freya-nle-app` types in scope.
pub(super) fn timeline_clip_style(clip_type: &str) -> Option<(Color, bool)> {
    let [red, green, blue] = match clip_type {
        "audio" => [0xd0, 0x40, 0x30],
        "camera" | "camera_control" | "scene" => [0x30, 0xb0, 0xc0],
        "counter" | "flare" | "lens_flare_object" | "pie_shape" | "polygon_shape"
        | "radial_lines" | "star" | "track_line" => [0x3b, 0x82, 0xf6],
        _ => return None,
    };
    let luma = 0.299 * f32::from(red) + 0.587 * f32::from(green) + 0.114 * f32::from(blue);
    // A half-bright fill takes dark text: the cyan control colour sits just
    // under 0.55, so the old 0.6 cutoff left white text on it (~2:1 contrast).
    Some((Color::from_rgb_u8(red, green, blue), luma > 0.5 * 255.0))
}

/// Parses a stored layer background (`#rrggbb`/`#aarrggbb`) into a Slint colour.
///
/// The document normalizer already canonicalizes the text, so this only has to
/// accept the two forms it can produce.
pub(super) fn layer_background_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    let parsed = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(Color::from_rgb_u8(
            ((parsed >> 16) & 0xff) as u8,
            ((parsed >> 8) & 0xff) as u8,
            (parsed & 0xff) as u8,
        )),
        8 => Some(Color::from_argb_u8(
            ((parsed >> 24) & 0xff) as u8,
            ((parsed >> 16) & 0xff) as u8,
            ((parsed >> 8) & 0xff) as u8,
            (parsed & 0xff) as u8,
        )),
        _ => None,
    }
}

/// The label the object-kind filter shows for one kind id.
fn object_kind_label(kind: &str) -> String {
    if kind.is_empty() {
        return localized("All kinds", "全部类型", "すべての種類").to_owned();
    }
    localized_effect_metadata(kind).into_owned()
}

pub(super) fn sync_project_tabs(
    main: &MainWindow,
    timeline: &TimelineWindow,
    model: &ApplicationModel,
) {
    let tabs = model
        .tabs()
        .into_iter()
        .map(|tab| ProjectTabData {
            name: SharedString::from(tab.name),
            dirty: tab.dirty,
        })
        .collect::<Vec<_>>();
    let current = model
        .current_project_index()
        .map_or(-1, |index| index as i32);
    update_vec_model(&main.get_project_tabs(), tabs.clone());
    main.set_current_project(current);
    // The workspace menu item names the action the other layout would take.
    main.set_merged_workspace(timeline.get_preview_visible());
    // The merged timeline shows the same strip, so it can stand in for the
    // preview window.
    update_vec_model(&timeline.get_project_tabs(), tabs);
    timeline.set_current_project(current);
}

pub(super) fn update_vec_model<T: Clone + 'static>(model: &ModelRc<T>, rows: Vec<T>) {
    let Some(model) = model.as_any().downcast_ref::<VecModel<T>>() else {
        eprintln!("UI list was replaced with a non-VecModel; skipping sync");
        return;
    };
    let common_rows = model.row_count().min(rows.len());
    for (index, row) in rows.iter().take(common_rows).cloned().enumerate() {
        model.set_row_data(index, row);
    }
    if rows.len() > common_rows {
        model.extend(rows.into_iter().skip(common_rows));
    } else {
        while model.row_count() > rows.len() {
            model.remove(rows.len());
        }
    }
}

/// Projects the edited scene's frame marks and whether the playhead sits on one.
///
/// Both windows own the same menu bar, so both get the list; the ruler reads the
/// frame numbers and the mark submenu reads the preformatted labels.
fn sync_marks(main: &MainWindow, timeline: &TimelineWindow, model: &ApplicationModel) {
    let Some(workspace) = model.current_workspace() else {
        update_vec_model(&main.get_marks(), Vec::<TimelineMarkData>::new());
        update_vec_model(&timeline.get_marks(), Vec::<TimelineMarkData>::new());
        main.set_mark_at_playhead(false);
        timeline.set_mark_at_playhead(false);
        return;
    };
    let rate = workspace
        .scene_settings(workspace.selected_scene())
        .map_or(60, |scene| scene_frame_rate(scene.fps));
    let marks = workspace
        .scene_marks()
        .into_iter()
        .map(|mark| TimelineMarkData {
            frame: mark.frame,
            label: SharedString::from(mark_label(mark.frame, rate, &mark.note)),
            note: SharedString::from(mark.note),
        })
        .collect::<Vec<_>>();
    let at_playhead = workspace.mark_at_playhead().is_some();
    update_vec_model(&main.get_marks(), marks.clone());
    update_vec_model(&timeline.get_marks(), marks);
    main.set_mark_at_playhead(at_playhead);
    timeline.set_mark_at_playhead(at_playhead);
}

pub(super) fn sync_transport(
    main: &MainWindow,
    timeline: &TimelineWindow,
    model: &ApplicationModel,
) {
    let Some(workspace) = model.current_workspace() else {
        return;
    };
    let duration = workspace.timeline_duration();
    main.set_playhead(workspace.playhead() as f32);
    main.set_duration(duration as f32);
    main.set_playing(workspace.is_playing());
    let speed_percent = (workspace.playback_speed() * 100.0)
        .round()
        .clamp(10.0, 400.0) as i32;
    main.set_playback_speed_percent(speed_percent);
    timeline.set_selected_clip_count(workspace.selected_clip_count() as i32);
    timeline.set_selection_has_group(workspace.selection_has_group());
    main.set_playback_speed_multiplier(SharedString::from(speed_multiplier_text(speed_percent)));
    main.set_status_text(status_line(workspace.status(), workspace.status_message()));
    main.set_frame_counter_text(SharedString::from(frame_counter_text(
        workspace.playhead(),
        duration,
    )));
    timeline.set_playhead(workspace.playhead());
    timeline.set_duration(workspace.timeline_view_duration());
    // The ruler is padded to the window's width; the transport shows the real
    // project length.
    timeline.set_playback_duration(duration);
    timeline.set_selected_layer(workspace.selected_layer());
    // The merged preview pane draws its own transport button state.
    timeline.set_playing(workspace.is_playing());
    main.set_playhead_frame(workspace.playhead());
    if let Some(scene) = workspace.scene_settings(workspace.selected_scene()) {
        timeline.set_grid_mode(SharedString::from(scene.grid_mode));
        timeline.set_grid_fps(finite_f32(scene.fps, 60.0));
        timeline.set_grid_bpm(finite_f32(scene.grid_bpm, 120.0));
        timeline.set_grid_offset(finite_f32(scene.grid_offset, 0.0));
        timeline.set_grid_interval(scene.grid_interval.max(1));
        timeline.set_grid_subdivision(scene.grid_subdivision.max(1));
        // The footer repeats what the scene list and the time gauge refer to:
        // name, pixel size and frame rate (AviUtl2's style.conf [Format]).
        let frame_rate = scene_frame_rate(scene.fps);
        timeline.set_scene_name(SharedString::from(scene.name));
        timeline.set_scene_resolution(SharedString::from(scene_resolution_text(
            scene.width,
            scene.height,
        )));
        timeline.set_scene_frame_rate(frame_rate);
        main.set_frame_rate(frame_rate);
    } else {
        timeline.set_scene_name(SharedString::default());
        timeline.set_scene_resolution(SharedString::default());
        timeline.set_scene_frame_rate(60);
        main.set_frame_rate(60);
    }
    // Mark state follows the playhead, so it belongs to the transport sync
    // rather than to the project-wide sync.
    sync_marks(main, timeline, model);
}

/// Nominal frame rate for the timecode readings.
///
/// The timecode counts whole frames per second, so a fractional rate is
/// rounded the way a video editor labels its own ruler. Zero, negative and
/// non-finite rates fall back to 60 instead of dividing by zero.
pub(super) fn scene_frame_rate(fps: f64) -> i32 {
    if !fps.is_finite() || fps <= 0.0 {
        return 60;
    }
    fps.round().clamp(1.0, 1000.0) as i32
}

/// `1920x1080` for the footer's resolution reading.
pub(super) fn scene_resolution_text(width: i32, height: i32) -> String {
    format!("{width}x{height}")
}

/// Format the playback speed the way Qt's SpinBox does.
///
/// Qt renders the transport speed as a multiplier with one decimal, e.g. `1.0x`
/// (`MainWindow.qml:1143-1145`). Slint's `SpinBox` only edits an integer and has
/// no text formatter, so the box keeps the percent value and this multiplier is
/// projected alongside it.
pub(super) fn speed_multiplier_text(percent: i32) -> String {
    format!("{:.1}x", f64::from(percent) / 100.0)
}

/// Format the transport frame counter the way Qt does.
///
/// Qt pads the current frame to the total's digit count and lets the label size
/// itself, so the counter never loses leading digits and keeps a stable width
/// while the numbers change (`MainWindow.qml:1035-1049`).
pub(super) fn frame_counter_text(playhead: i32, duration: i32) -> String {
    let total = duration.max(0);
    let current = playhead.max(0);
    let width = total.to_string().len();
    format!("{current:0>width$} / {total}")
}

/// `HH:MM:SS.FF` for a mark, matching `TimeFormat.timecode` in the ruler.
///
/// The mark menu and the note dialog show this reading instead of a raw frame
/// number, so a mark is recognisable away from the ruler.
pub(super) fn mark_timecode(frame: i32, rate: i32) -> String {
    let rate = rate.max(1);
    let frame = frame.max(0);
    let total_seconds = frame / rate;
    format!(
        "{:02}:{:02}:{:02}.{:02}",
        total_seconds / 3600,
        (total_seconds / 60) % 60,
        total_seconds % 60,
        frame % rate
    )
}

/// Menu label for one mark: its timecode, then the note when there is one.
///
/// Long notes are clipped because a menu entry has no ellipsis of its own; the
/// full note stays in the document and in the note dialog.
pub(super) fn mark_label(frame: i32, rate: i32, note: &str) -> String {
    const NOTE_CHARS: usize = 40;
    let time = mark_timecode(frame, rate);
    let note = note.trim();
    if note.is_empty() {
        return time;
    }
    let mut label = format!("{time} · ");
    label.extend(note.chars().take(NOTE_CHARS));
    if note.chars().count() > NOTE_CHARS {
        label.push('…');
    }
    label
}

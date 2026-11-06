//! Settings drafts, validation, and runtime application.
use crate::localization::{UiLanguage, current_ui_language, localized};
use crate::projection::{setting_string, update_vec_model};
use crate::shortcuts::{shortcut_setting, timeline_zoom_settings};
use crate::{
    MainWindow, ObjectSettingsWindow, ProjectSettingsWindow, SceneSettingsWindow,
    SystemPluginSettingData, SystemSettingsWindow, SystemShortcutSettingData, TimelineWindow,
};
use freya_nle_app::settings::SettingsStore;
use freya_nle_app::{
    ApplicationModel, MAX_TIMELINE_LAYERS, ProjectDefaults, ProjectSettingsInput,
    SceneSettingsInput,
};
use serde_json::{Map, Value};
use slint::{ComponentHandle, Model, SharedString};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

pub(super) const SYSTEM_THEME_VALUES: [&str; 3] = ["Dark", "Light", "System"];

pub(super) const SYSTEM_UI_LANGUAGE_VALUES: [&str; 4] =
    ["System", "English", "SimplifiedChinese", "Japanese"];

pub(super) const SYSTEM_PREVIEW_RENDER_SCALES: [f64; 4] = [1.0, 0.75, 0.5, 0.25];

pub(super) const SYSTEM_PREVIEW_MSAA_SAMPLES: [i32; 4] = [0, 2, 4, 8];

pub(super) const SYSTEM_BAKE_STRATEGIES: [&str; 2] = ["OnDemand", "FullBake"];

pub(super) const SYSTEM_EXPORT_VIDEO_CODECS: [&str; 3] = ["h264_vaapi", "hevc_vaapi", "libx264"];

pub(super) const SYSTEM_EXPORT_AUDIO_CODECS: [&str; 4] = ["aac", "opus", "flac", "pcm_s16le"];

pub(super) const SYSTEM_AUDIO_BLOCK_SIZES: [i32; 6] = [256, 512, 1024, 2048, 4096, 8192];

pub(super) const SYSTEM_PLUGIN_FORMATS: [&str; 11] = [
    "LADSPA", "DSSI", "LV2", "VST2", "VST3", "CLAP", "SF2", "SFZ", "JSFX", "Effects", "Objects",
];

pub(super) const SYSTEM_SHORTCUT_ROWS: [(&str, &str); 38] = [
    ("project.new", "Ctrl+N"),
    ("project.open", "Ctrl+O"),
    ("project.save", "Ctrl+S"),
    ("project.saveAs", "Ctrl+Shift+S"),
    ("project.export", "Ctrl+E"),
    ("app.quit", "Ctrl+Q"),
    ("app.settings", "Ctrl+P"),
    ("edit.undo", "Ctrl+Z"),
    ("edit.redo", "Ctrl+Shift+Z"),
    ("edit.cut", "Ctrl+X"),
    ("edit.copy", "Ctrl+C"),
    ("edit.paste", "Ctrl+V"),
    ("edit.delete", "Delete"),
    ("edit.duplicate", "Ctrl+D"),
    ("transport.playPause", "Space"),
    ("transport.nextFrame", "Right"),
    ("transport.prevFrame", "Left"),
    ("transport.jumpStart", "Home"),
    ("transport.jumpEnd", "End"),
    ("view.zoomIn", "Ctrl++"),
    ("view.zoomOut", "Ctrl+-"),
    ("view.timeline", "F3"),
    ("view.objectSettings", "F4"),
    ("project.settings", "Alt+Enter"),
    ("timeline.split", "S"),
    ("timeline.moveUp", "Alt+Up"),
    ("timeline.moveDown", "Alt+Down"),
    ("timeline.nudgeLeft", "Alt+Left"),
    ("timeline.nudgeRight", "Alt+Right"),
    ("timeline.addScene", "Ctrl+T"),
    ("timeline.sceneSettings", "Alt+S"),
    ("timeline.removeScene", "Ctrl+Shift+Delete"),
    ("timeline.layerLock", "Ctrl+L"),
    ("timeline.layerHide", "Ctrl+H"),
    ("timeline.toggleMark", "M"),
    ("timeline.markNote", "Shift+M"),
    ("timeline.nextMark", "Ctrl+Alt+Right"),
    ("timeline.previousMark", "Ctrl+Alt+Left"),
];

pub(super) fn system_theme_index(settings: &SettingsStore) -> i32 {
    choice_index_str(
        &setting_string(settings, "theme", "Dark"),
        &SYSTEM_THEME_VALUES,
        0,
    )
}

pub(super) fn timeline_maximum_layers(settings: &SettingsStore) -> i32 {
    settings
        .i32_value("timelineMaxLayers", 128)
        .clamp(1, MAX_TIMELINE_LAYERS)
}

pub(super) fn timeline_last_layer(window: &TimelineWindow) -> i32 {
    window.get_maximum_layers().clamp(1, MAX_TIMELINE_LAYERS) - 1
}

pub(super) fn sync_timeline_runtime_settings(
    main: &MainWindow,
    timeline: &TimelineWindow,
    object_settings: &ObjectSettingsWindow,
    settings: &SettingsStore,
) {
    let menu_shortcuts = slint::ModelRc::new(slint::VecModel::from(
        SYSTEM_SHORTCUT_ROWS
            .iter()
            .map(|(key, fallback)| {
                crate::shortcuts::native_shortcut(&shortcut_setting(settings, key, fallback))
            })
            .collect::<Vec<_>>(),
    ));
    // Both windows own a menu bar, so both need the same accelerator table.
    main.set_menu_shortcuts(menu_shortcuts.clone());
    timeline.set_menu_shortcuts(menu_shortcuts);
    let header_height = settings
        .i32_value("timelineHeaderHeight", 28)
        .clamp(16, 100);
    main.set_project_tab_height(header_height);
    // The merged timeline draws the same project strip, so it uses the same
    // height instead of its own default.
    timeline.set_project_tab_height(header_height);
    timeline.set_timeline_header_height(header_height);
    timeline
        .set_timeline_track_height(settings.i32_value("timelineTrackHeight", 32).clamp(16, 100));
    timeline
        .set_timeline_ruler_height(settings.i32_value("timelineRulerHeight", 32).clamp(16, 100));
    timeline.set_maximum_layers(timeline_maximum_layers(settings));
    timeline.set_layer_header_width(
        settings
            .i32_value("timelineLayerHeaderWidth", 96)
            .clamp(40, 300),
    );
    timeline.set_clip_resize_handle_width(
        settings
            .i32_value("timelineClipResizeHandleWidth", 10)
            .clamp(4, 40),
    );
    timeline.set_minimum_clip_duration_frames(
        settings.i32_value("minClipDurationFrames", 5).clamp(1, 100),
    );
    // AviUtl2 keeps the preview above the layer area; the merged pane follows
    // the same preference here and can be hidden again from the View menu.
    timeline.set_preview_visible(settings.bool_value("timelinePreviewVisible", false));
    // The preview window's View menu names the layout action, so it has to
    // know what the timeline is doing.
    main.set_merged_workspace(timeline.get_preview_visible());
    timeline.set_preview_pane_height(preview_pane_height(timeline, settings) as f32);
    let skimming_enabled = settings.bool_value("enableTimelineSkimming", true);
    timeline.set_timeline_skimming_enabled(skimming_enabled);
    if !skimming_enabled {
        timeline.set_skimmer_visible(false);
    }
    object_settings.set_sidebar_on_right(settings.bool_value("settingDialogSidebarRight", false));
}

/// Tallest the merged preview pane may get in this window.
///
/// The pane competes with the ruler and the layer area for one window, so the
/// ceiling is 720px and never more than the window height minus the 240px the
/// rest of the timeline needs. The 120px floor wins in a window too short for
/// the subtraction, because the pane cannot be smaller than that either.
pub(super) fn preview_pane_max_height(timeline: &TimelineWindow) -> i32 {
    let window = timeline.window();
    let available = window.size().to_logical(window.scale_factor()).height as i32 - 240;
    available.clamp(120, 720)
}

/// Height for the merged preview pane.
///
/// The saved value, bounded by the window like the divider drag is.
pub(super) fn preview_pane_height(timeline: &TimelineWindow, settings: &SettingsStore) -> i32 {
    let requested = settings.i32_value("timelinePreviewHeight", 320);
    requested.clamp(120, preview_pane_max_height(timeline))
}

/// Persists the merged-preview preferences.
///
/// The View-menu toggle, the divider drag, the height menu items and the
/// workspace action all change one of these two values; keeping the atomic
/// write and the clamping here stops the copies from drifting apart.
pub(super) fn save_timeline_preview_settings(
    settings: &RefCell<SettingsStore>,
    visible: Option<bool>,
    height: Option<f32>,
) -> Result<(), String> {
    let mut snapshot = settings.borrow().snapshot();
    if let Some(visible) = visible {
        snapshot.insert(
            "timelinePreviewVisible".to_owned(),
            serde_json::json!(visible),
        );
    }
    if let Some(height) = height {
        snapshot.insert(
            "timelinePreviewHeight".to_owned(),
            serde_json::json!(height.round().clamp(120.0, 720.0) as i32),
        );
    }
    settings.borrow_mut().apply(snapshot)
}

/// Per-effect parameter defaults used when a new object or effect is created.
pub(super) fn effect_defaults(settings: &SettingsStore) -> Map<String, Value> {
    settings
        .value("effectDefaults")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Per-object-type creation defaults: `{objectId: {effectId: params}}`.
///
/// AviUtl2 saves a media object's defaults as its input settings combined with
/// the output (standard drawing), so a new object of that type starts at the
/// position, scale and appearance the user last configured. Effect defaults
/// stay separate because they apply to any object that gains that effect.
pub(super) fn object_defaults(settings: &SettingsStore) -> Map<String, Value> {
    settings
        .value("objectDefaults")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// The defaults for one object type, ready to merge over the effect defaults.
pub(super) fn object_defaults_for(settings: &SettingsStore, object_id: &str) -> Map<String, Value> {
    object_defaults(settings)
        .remove(object_id)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

pub(super) fn save_object_default(
    settings: &mut SettingsStore,
    object_id: &str,
    effects: &Map<String, Value>,
) -> Result<(), String> {
    if object_id.is_empty() || effects.is_empty() {
        return Err("There is no object default to save".to_owned());
    }
    let mut root = settings.snapshot();
    let mut defaults = root
        .get("objectDefaults")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut entry = defaults
        .get(object_id)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // Each effect's parameters are replaced as a set: the caller always saves
    // the whole effect, so merging key by key would keep a parameter the user
    // has since removed from the catalog.
    for (effect_id, params) in effects {
        entry.insert(effect_id.clone(), params.clone());
    }
    defaults.insert(object_id.to_owned(), Value::Object(entry));
    root.insert("objectDefaults".to_owned(), Value::Object(defaults));
    settings.apply(root)
}

pub(super) fn clear_object_default(
    settings: &mut SettingsStore,
    object_id: &str,
) -> Result<(), String> {
    let mut root = settings.snapshot();
    let empty = {
        let Some(defaults) = root
            .get_mut("objectDefaults")
            .and_then(Value::as_object_mut)
        else {
            return Ok(());
        };
        defaults.remove(object_id);
        defaults.is_empty()
    };
    if empty {
        root.remove("objectDefaults");
    }
    settings.apply(root)
}

pub(super) fn save_effect_default(
    settings: &mut SettingsStore,
    effect_id: &str,
    params: &Map<String, Value>,
) -> Result<(), String> {
    let mut root = settings.snapshot();
    let mut defaults = root
        .get("effectDefaults")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    defaults.insert(effect_id.to_owned(), Value::Object(params.clone()));
    root.insert("effectDefaults".to_owned(), Value::Object(defaults));
    settings.apply(root)
}

pub(super) fn clear_effect_default(
    settings: &mut SettingsStore,
    effect_id: &str,
) -> Result<(), String> {
    let mut root = settings.snapshot();
    let empty = {
        let Some(defaults) = root
            .get_mut("effectDefaults")
            .and_then(Value::as_object_mut)
        else {
            return Ok(());
        };
        defaults.remove(effect_id);
        defaults.is_empty()
    };
    if empty {
        root.remove("effectDefaults");
    }
    settings.apply(root)
}

/// The user's named colours: `{name: "#rrggbb"}`.
///
/// AviUtl2's 色設定 gives each palette entry a preset name, and `<#name>` in a
/// text resolves through it. The palette is application-wide like the text
/// presets, so a project written on one machine keeps its colour names.
const TEXT_PALETTE_KEY: &str = "textPalette";

/// One palette entry as the interface sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaletteEntry {
    pub name: String,
    /// `#rrggbb`, always six digits with no alpha.
    pub color: String,
}

pub(super) fn text_palette(settings: &SettingsStore) -> Vec<PaletteEntry> {
    let mut entries = settings
        .value(TEXT_PALETTE_KEY)
        .and_then(Value::as_object)
        .map(|palette| {
            palette
                .iter()
                .filter_map(|(name, color)| {
                    let color = valid_palette_color(color.as_str()?)?;
                    Some(PaletteEntry {
                        name: name.clone(),
                        color,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

/// `rrggbb` or `#rrggbb`, normalized to the latter.
///
/// An entry with alpha would make `<#name>` mean something the text colour
/// already carries separately, so only six digits are accepted.
fn valid_palette_color(color: &str) -> Option<String> {
    let hex = color.trim().trim_start_matches('#');
    (hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_lowercase()))
}

pub(super) fn valid_palette_name(name: &str) -> Option<&str> {
    let name = name.trim();
    // `>` ends a directive and `<` starts one, so either would make the name
    // unreachable from `<#name>`.
    let valid = !name.is_empty()
        && name.chars().count() <= 64
        && !name
            .chars()
            .any(|character| matches!(character, '<' | '>' | ',' | '#') || character.is_control());
    valid.then_some(name)
}

pub(super) fn save_palette_color(
    settings: &mut SettingsStore,
    name: &str,
    color: &str,
) -> Result<(), String> {
    let name = valid_palette_name(name)
        .ok_or_else(|| "A colour name cannot be empty or contain <, >, , or #".to_owned())?;
    let color = valid_palette_color(color)
        .ok_or_else(|| "A palette colour must be written as #rrggbb".to_owned())?;
    let mut root = settings.snapshot();
    let mut palette = root
        .get(TEXT_PALETTE_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    palette.insert(name.to_owned(), Value::String(color));
    root.insert(TEXT_PALETTE_KEY.to_owned(), Value::Object(palette));
    settings.apply(root)?;
    sync_text_palette(settings);
    Ok(())
}

pub(super) fn delete_palette_color(settings: &mut SettingsStore, name: &str) -> Result<(), String> {
    let mut root = settings.snapshot();
    let empty = {
        let Some(palette) = root
            .get_mut(TEXT_PALETTE_KEY)
            .and_then(Value::as_object_mut)
        else {
            return Ok(());
        };
        palette.remove(name);
        palette.is_empty()
    };
    if empty {
        root.remove(TEXT_PALETTE_KEY);
    }
    settings.apply(root)?;
    sync_text_palette(settings);
    Ok(())
}

/// Hands the stored palette to the text parser.
pub(super) fn sync_text_palette(settings: &SettingsStore) {
    let entries = text_palette(settings)
        .into_iter()
        .filter_map(|entry| {
            let color = freya_nle_core::api::parse_text_color(&entry.color)?;
            Some(freya_nle_core::api::PaletteColor {
                name: entry.name,
                color,
            })
        })
        .collect::<Vec<_>>();
    freya_nle_core::api::set_text_palette(entries);
}

/// Parameter names a text preset can carry: the text effect keys the text
/// control characters are able to change, so a preset and `<$name>` agree.
pub(super) const TEXT_PRESET_PARAMETERS: [&str; 8] = [
    "fontFamily",
    "fontSize",
    "color",
    "outlineColor",
    "fontBold",
    "fontItalic",
    "letterSpacing",
    "lineSpacing",
];

const TEXT_PRESETS_KEY: &str = "textPresets";

pub(super) fn text_preset_names(settings: &SettingsStore) -> Vec<String> {
    let mut names = settings
        .value(TEXT_PRESETS_KEY)
        .and_then(Value::as_object)
        .map(|presets| presets.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    names.sort_unstable();
    names
}

fn valid_text_preset_name(name: &str) -> Option<&str> {
    let name = name.trim();
    // The directive ends at `>`, so a name containing `>` could never be
    // addressed; `$` and control characters are rejected for the same reason.
    let valid = !name.is_empty()
        && name.chars().count() <= 64
        && !name
            .chars()
            .any(|character| matches!(character, '<' | '>' | '$') || character.is_control());
    valid.then_some(name)
}

pub(super) fn save_text_preset(
    settings: &mut SettingsStore,
    name: &str,
    params: &Map<String, Value>,
) -> Result<(), String> {
    let name = valid_text_preset_name(name)
        .ok_or_else(|| "A preset name cannot be empty or contain <, > or $".to_owned())?;
    if params.is_empty() {
        return Err("The text object has no style settings to save".to_owned());
    }
    let mut root = settings.snapshot();
    let mut presets = root
        .get(TEXT_PRESETS_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    presets.insert(name.to_owned(), Value::Object(params.clone()));
    root.insert(TEXT_PRESETS_KEY.to_owned(), Value::Object(presets));
    settings.apply(root)?;
    sync_text_presets(settings);
    Ok(())
}

pub(super) fn delete_text_preset(settings: &mut SettingsStore, name: &str) -> Result<(), String> {
    let mut root = settings.snapshot();
    let empty = {
        let Some(presets) = root
            .get_mut(TEXT_PRESETS_KEY)
            .and_then(Value::as_object_mut)
        else {
            return Ok(());
        };
        presets.remove(name);
        presets.is_empty()
    };
    if empty {
        root.remove(TEXT_PRESETS_KEY);
    }
    settings.apply(root)?;
    sync_text_presets(settings);
    Ok(())
}

/// Hands the stored presets to the text parser.
pub(super) fn sync_text_presets(settings: &SettingsStore) {
    let presets = settings
        .value(TEXT_PRESETS_KEY)
        .and_then(Value::as_object)
        .map(|presets| {
            presets
                .iter()
                .filter_map(|(name, params)| {
                    let params = params.as_object()?;
                    preset_from_params(name, params)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    freya_nle_core::api::set_text_presets(presets);
}

fn preset_from_params(
    name: &str,
    params: &Map<String, Value>,
) -> Option<freya_nle_core::api::TextPreset> {
    let preset = freya_nle_core::api::TextPreset {
        name: name.to_owned(),
        font_family: params
            .get("fontFamily")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|family| !family.is_empty())
            .map(str::to_owned),
        font_size: params
            .get("fontSize")
            .and_then(Value::as_f64)
            .filter(|size| size.is_finite() && *size > 0.0)
            .map(|size| size as f32),
        color: params
            .get("color")
            .and_then(Value::as_str)
            .and_then(freya_nle_core::api::parse_text_color),
        outline_color: params
            .get("outlineColor")
            .and_then(Value::as_str)
            .and_then(freya_nle_core::api::parse_text_color),
        bold: params.get("fontBold").and_then(Value::as_bool),
        italic: params.get("fontItalic").and_then(Value::as_bool),
        letter_spacing: params
            .get("letterSpacing")
            .and_then(Value::as_f64)
            .filter(|spacing| spacing.is_finite())
            .map(|spacing| spacing as f32),
        line_spacing: params
            .get("lineSpacing")
            .and_then(Value::as_f64)
            .filter(|spacing| spacing.is_finite() && *spacing >= 0.0)
            .map(|spacing| spacing as f32),
    };
    let empty = preset.font_family.is_none()
        && preset.font_size.is_none()
        && preset.color.is_none()
        && preset.outline_color.is_none()
        && preset.bold.is_none()
        && preset.italic.is_none()
        && preset.letter_spacing.is_none()
        && preset.line_spacing.is_none();
    (!empty).then_some(preset)
}

pub(super) fn project_defaults(settings: &SettingsStore) -> ProjectDefaults {
    let fps = settings.f64_value("defaultProjectFps", 60.0);
    ProjectDefaults {
        width: settings
            .i32_value("defaultProjectWidth", 1_920)
            .clamp(1, 16_000),
        height: settings
            .i32_value("defaultProjectHeight", 1_080)
            .clamp(1, 16_000),
        fps: if fps.is_finite() {
            fps.clamp(1.0, 240.0)
        } else {
            60.0
        },
        sample_rate: settings
            .i32_value("defaultProjectSampleRate", 48_000)
            .clamp(8_000, 192_000),
        duration: settings
            .i32_value("defaultProjectFrames", 3_600)
            .clamp(1, 1_000_000),
        enable_snap: settings.bool_value("enableSnap", true),
        magnetic_snap_range: settings.i32_value("magneticSnapRange", 10).clamp(1, 100),
    }
}

pub(super) fn sync_project_settings(window: &ProjectSettingsWindow, input: &ProjectSettingsInput) {
    window.set_project_width(input.width);
    window.set_project_height(input.height);
    window.set_project_fps(SharedString::from(input.fps.to_string()));
    window.set_project_sample_rate(input.sample_rate);
}

pub(super) fn sync_scene_settings(
    window: &SceneSettingsWindow,
    creation_mode: bool,
    scene_id: i32,
    input: &SceneSettingsInput,
) {
    window.set_creation_mode(creation_mode);
    window.set_target_scene_id(scene_id);
    window.set_scene_name(SharedString::from(input.name.clone()));
    window.set_scene_label(SharedString::from(input.label.clone()));
    window.set_scene_width(input.width);
    window.set_scene_height(input.height);
    window.set_scene_fps(SharedString::from(input.fps.to_string()));
    window.set_scene_duration(input.duration);
    window.set_grid_mode_index(match input.grid_mode.as_str() {
        "BPM" => 1,
        "Frame" => 2,
        _ => 0,
    });
    window.set_grid_bpm(SharedString::from(input.grid_bpm.to_string()));
    window.set_grid_offset(SharedString::from(input.grid_offset.to_string()));
    window.set_grid_interval(SharedString::from(input.grid_interval.to_string()));
    window.set_grid_subdivision(SharedString::from(input.grid_subdivision.to_string()));
    window.set_enable_snap(input.enable_snap);
    window.set_magnetic_snap_range(input.magnetic_snap_range);
}

pub(super) fn sync_system_settings(window: &SystemSettingsWindow, settings: &SettingsStore) {
    // The size line is informational, so an unreadable store simply shows the
    // disk usage as zero rather than blocking the page.
    window.set_backup_size_text(SharedString::from(backup_size_line(settings)));
    let defaults = project_defaults(settings);
    window.set_confirm_unsaved(settings.bool_value("showConfirmOnClose", true));
    window.set_auto_backup(settings.bool_value("enableAutoBackup", true));
    window.set_backup_interval(settings.i32_value("backupInterval", 5).clamp(1, 60));
    window.set_backup_history_enabled(settings.bool_value("backupHistoryEnabled", true));
    window.set_backup_history_limit(settings.i32_value("backupHistoryLimit", 20).clamp(1, 100));
    window.set_backup_history_total_limit(settings.i32_value(
        "backupHistoryTotalLimitMb",
        freya_nle_app::backup::DEFAULT_BACKUP_TOTAL_LIMIT_MB as i32,
    ));
    window.set_backup_history_directory(SharedString::from(
        settings
            .value("backupHistoryDirectory")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    ));
    window
        .set_recent_project_max_count(settings.i32_value("recentProjectMaxCount", 10).clamp(1, 50));
    window.set_undo_count(settings.i32_value("undoCount", 32).clamp(1, 1_000));
    window.set_max_image_size(
        settings
            .i32_value("maxImageSize", 8_192)
            .clamp(1_024, 16_384),
    );
    window.set_cache_size(settings.i32_value("cacheSize", 512).clamp(64, 8_192));
    window.set_preview_render_scale_index(choice_index_f64(
        settings.f64_value("previewRenderScale", 1.0),
        &SYSTEM_PREVIEW_RENDER_SCALES,
        0,
    ));
    window.set_preview_msaa_index(choice_index_i32(
        settings.i32_value("previewMsaaSamples", 0),
        &SYSTEM_PREVIEW_MSAA_SAMPLES,
        0,
    ));
    window.set_bake_strategy_index(choice_index_str(
        &setting_string(settings, "bakeStrategy", "OnDemand"),
        &SYSTEM_BAKE_STRATEGIES,
        0,
    ));
    window.set_on_demand_prefetch_frames(
        settings
            .i32_value("onDemandPrefetchFrames", 30)
            .clamp(0, 600),
    );
    window.set_enable_timeline_skimming(settings.bool_value("enableTimelineSkimming", true));
    window.set_timeline_preview_visible(settings.bool_value("timelinePreviewVisible", false));
    window.set_timeline_track_height(settings.i32_value("timelineTrackHeight", 32).clamp(16, 100));
    window.set_timeline_header_height(
        settings
            .i32_value("timelineHeaderHeight", 28)
            .clamp(16, 100),
    );
    window
        .set_setting_dialog_sidebar_right(settings.bool_value("settingDialogSidebarRight", false));
    window.set_timeline_ruler_height(settings.i32_value("timelineRulerHeight", 32).clamp(16, 100));
    window.set_timeline_max_layers(
        settings
            .i32_value("timelineMaxLayers", 128)
            .clamp(1, MAX_TIMELINE_LAYERS),
    );
    window.set_timeline_layer_header_width(
        settings
            .i32_value("timelineLayerHeaderWidth", 96)
            .clamp(40, 300),
    );
    window.set_timeline_clip_resize_handle_width(
        settings
            .i32_value("timelineClipResizeHandleWidth", 10)
            .clamp(4, 40),
    );
    window
        .set_min_clip_duration_frames(settings.i32_value("minClipDurationFrames", 5).clamp(1, 100));
    let (zoom_minimum, zoom_maximum, zoom_step) = timeline_zoom_settings(settings);
    window.set_timeline_zoom_min(zoom_minimum.round() as i32);
    window.set_timeline_zoom_max(zoom_maximum.round() as i32);
    window.set_timeline_zoom_step(zoom_step.round() as i32);
    window.set_theme_index(choice_index_str(
        &setting_string(settings, "theme", "Dark"),
        &SYSTEM_THEME_VALUES,
        0,
    ));
    window.set_language_index(choice_index_str(
        &setting_string(settings, "uiLanguage", "System"),
        &SYSTEM_UI_LANGUAGE_VALUES,
        0,
    ));
    window.set_default_project_width(defaults.width);
    window.set_default_project_height(defaults.height);
    window.set_default_project_fps(SharedString::from(defaults.fps.to_string()));
    window.set_default_project_frames(defaults.duration);
    window.set_default_project_sample_rate(defaults.sample_rate);
    window.set_default_clip_duration(
        settings
            .i32_value("defaultClipDuration", 100)
            .clamp(1, 100_000),
    );
    window.set_export_video_codec_index(choice_index_str(
        &setting_string(settings, "exportDefaultCodec", "libx264"),
        &SYSTEM_EXPORT_VIDEO_CODECS,
        2,
    ));
    window.set_export_default_bitrate_mbps(
        settings
            .i32_value("exportDefaultBitrateMbps", 15)
            .clamp(1, 500),
    );
    window.set_export_default_crf(settings.i32_value("exportDefaultCrf", 20).clamp(0, 51));
    window.set_export_image_quality(settings.i32_value("exportImageQuality", 95).clamp(0, 100));
    window.set_export_sequence_padding(settings.i32_value("exportSequencePadding", 6).clamp(2, 10));
    let audio_codec = setting_string(settings, "exportDefaultAudioCodec", "aac");
    window.set_export_audio_codec_index(if audio_codec == "libopus" {
        1
    } else {
        choice_index_str(&audio_codec, &SYSTEM_EXPORT_AUDIO_CODECS, 0)
    });
    window.set_export_default_audio_bitrate_kbps(
        settings
            .i32_value("exportDefaultAudioBitrateKbps", 192)
            .clamp(32, 1_536),
    );
    window.set_export_frame_grab_timeout_ms(
        settings
            .i32_value("exportFrameGrabTimeoutMs", 2_000)
            .clamp(100, 10_000),
    );
    window
        .set_export_progress_interval(settings.i32_value("exportProgressInterval", 5).clamp(1, 60));
    window.set_export_encoder_queue_mb(
        settings
            .i32_value("exportEncoderQueueMB", 128)
            .clamp(16, 1_024),
    );
    window.set_video_decoder_index_reserve(
        settings
            .i32_value("videoDecoderIndexReserve", 108_000)
            .clamp(1_000, 1_000_000),
    );
    window.set_video_decoder_min_cache_mb(
        settings
            .i32_value("videoDecoderMinCacheMB", 64)
            .clamp(16, 4_096),
    );
    window.set_hw_frame_pool_size(settings.i32_value("hwFramePoolSize", 32).clamp(1, 256));
    window.set_audio_plugin_block_size_index(choice_index_i32(
        settings.i32_value("audioPluginMaxBlockSize", 4_096),
        &SYSTEM_AUDIO_BLOCK_SIZES,
        4,
    ));
    window.set_lua_hook_interval_ms(settings.i32_value("luaHookIntervalMs", 16).clamp(1, 1_000));
    window.set_lua_hot_reload(settings.bool_value("luaHotReload", false));

    update_vec_model(
        &window.get_plugin_settings(),
        SYSTEM_PLUGIN_FORMATS
            .iter()
            .map(|format| SystemPluginSettingData {
                format_name: SharedString::from(*format),
                enabled: settings.bool_value(&format!("pluginEnable{format}"), true),
                paths: SharedString::from(plugin_paths_text(settings, format)),
            })
            .collect(),
    );
    window.set_shortcut_validation_status(SharedString::new());
    update_vec_model(
        &window.get_shortcut_settings(),
        SYSTEM_SHORTCUT_ROWS
            .iter()
            .map(|(action_id, fallback)| SystemShortcutSettingData {
                action_id: SharedString::from(*action_id),
                value: SharedString::from(shortcut_setting(settings, action_id, fallback)),
            })
            .collect(),
    );
}

fn choice_index_str(value: &str, choices: &[&str], fallback: usize) -> i32 {
    choices
        .iter()
        .position(|choice| *choice == value)
        .unwrap_or(fallback) as i32
}

fn choice_index_i32(value: i32, choices: &[i32], fallback: usize) -> i32 {
    choices
        .iter()
        .position(|choice| *choice == value)
        .unwrap_or(fallback) as i32
}

fn choice_index_f64(value: f64, choices: &[f64], fallback: usize) -> i32 {
    choices
        .iter()
        .position(|choice| (*choice - value).abs() <= f64::EPSILON)
        .unwrap_or(fallback) as i32
}

/// `12.4 MB of 512 MB` summary for the backup budget row.
fn backup_size_line(settings: &SettingsStore) -> String {
    let used = freya_nle_app::backup::stored_bytes();
    let limit = settings
        .i32_value(
            "backupHistoryTotalLimitMb",
            freya_nle_app::backup::DEFAULT_BACKUP_TOTAL_LIMIT_MB as i32,
        )
        .clamp(
            freya_nle_app::backup::MIN_BACKUP_TOTAL_LIMIT_MB as i32,
            freya_nle_app::backup::MAX_BACKUP_TOTAL_LIMIT_MB as i32,
        );
    match current_ui_language() {
        crate::localization::UiLanguage::SimplifiedChinese => {
            format!("已用 {} / 上限 {limit} MB", backup_size_text(used))
        }
        crate::localization::UiLanguage::Japanese => {
            format!("{} 使用 / 上限 {limit} MB", backup_size_text(used))
        }
        _ => format!("{} of {limit} MB in use", backup_size_text(used)),
    }
}

fn backup_size_text(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn plugin_paths_text(settings: &SettingsStore, format: &str) -> String {
    settings
        .value(&format!("pluginPaths{format}"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn apply_runtime_settings(model: &mut ApplicationModel, settings: &SettingsStore) {
    let backup_directory = settings
        .value("backupHistoryDirectory")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    model.set_backup_directory(freya_nle_app::backup::backup_root_from_setting(
        backup_directory,
    ));
    let backup_minutes = settings.i32_value("backupInterval", 5).clamp(1, 24 * 60) as u64;
    model.set_recovery_interval(Duration::from_secs(backup_minutes * 60));
    model.set_auto_backup_enabled(settings.bool_value("enableAutoBackup", true));
    model.set_backup_history(
        settings.bool_value("backupHistoryEnabled", true),
        settings.i32_value("backupHistoryLimit", 20).clamp(1, 100) as usize,
        settings
            .i32_value(
                "backupHistoryTotalLimitMb",
                freya_nle_app::backup::DEFAULT_BACKUP_TOTAL_LIMIT_MB as i32,
            )
            .clamp(
                freya_nle_app::backup::MIN_BACKUP_TOTAL_LIMIT_MB as i32,
                freya_nle_app::backup::MAX_BACKUP_TOTAL_LIMIT_MB as i32,
            ) as u64,
    );
    model.set_undo_limit(settings.i32_value("undoCount", 32).max(1) as usize);
}

pub(super) fn apply_system_settings(
    window: &SystemSettingsWindow,
    settings: &Rc<RefCell<SettingsStore>>,
    model: &Rc<RefCell<ApplicationModel>>,
) -> Result<(), String> {
    let replacement = system_settings_replacement(window, settings.borrow().snapshot())?;
    // A typed folder must be absolute: a relative one would resolve against
    // whatever directory the editor happened to start in.
    let directory = window.get_backup_history_directory();
    let directory = directory.trim();
    if !directory.is_empty() && !std::path::Path::new(directory).is_absolute() {
        return Err(localized(
            "The backup folder must be an absolute path.",
            "备份文件夹必须是绝对路径。",
            "バックアップフォルダは絶対パスで指定してください。",
        )
        .to_owned());
    }
    settings.borrow_mut().apply(replacement)?;
    apply_runtime_settings(&mut model.borrow_mut(), &settings.borrow());
    Ok(())
}

fn system_settings_replacement(
    window: &SystemSettingsWindow,
    mut replacement: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let fps = parse_required_f64(
        &window.get_default_project_fps(),
        localized("Default frame rate", "默认帧率", "既定のフレームレート"),
        1.0,
        240.0,
    )?;
    let zoom_minimum = window.get_timeline_zoom_min().clamp(1, 400);
    let zoom_maximum = window.get_timeline_zoom_max().clamp(zoom_minimum, 1_000);
    for (key, value) in [
        (
            "showConfirmOnClose",
            serde_json::json!(window.get_confirm_unsaved()),
        ),
        (
            "enableAutoBackup",
            serde_json::json!(window.get_auto_backup()),
        ),
        (
            "backupInterval",
            serde_json::json!(window.get_backup_interval().clamp(1, 60)),
        ),
        (
            "backupHistoryEnabled",
            serde_json::json!(window.get_backup_history_enabled()),
        ),
        (
            "backupHistoryLimit",
            serde_json::json!(window.get_backup_history_limit().clamp(1, 100)),
        ),
        (
            "backupHistoryDirectory",
            serde_json::json!(window.get_backup_history_directory().trim()),
        ),
        (
            "backupHistoryTotalLimitMb",
            serde_json::json!(window.get_backup_history_total_limit().clamp(
                freya_nle_app::backup::MIN_BACKUP_TOTAL_LIMIT_MB as i32,
                freya_nle_app::backup::MAX_BACKUP_TOTAL_LIMIT_MB as i32,
            )),
        ),
        (
            "recentProjectMaxCount",
            serde_json::json!(window.get_recent_project_max_count().clamp(1, 50)),
        ),
        (
            "undoCount",
            serde_json::json!(window.get_undo_count().clamp(1, 1_000)),
        ),
        (
            "maxImageSize",
            serde_json::json!(window.get_max_image_size().clamp(1_024, 16_384)),
        ),
        (
            "cacheSize",
            serde_json::json!(window.get_cache_size().clamp(64, 8_192)),
        ),
        (
            "previewRenderScale",
            serde_json::json!(choice_f64(
                &SYSTEM_PREVIEW_RENDER_SCALES,
                window.get_preview_render_scale_index(),
                0,
            )),
        ),
        (
            "previewMsaaSamples",
            serde_json::json!(choice_i32(
                &SYSTEM_PREVIEW_MSAA_SAMPLES,
                window.get_preview_msaa_index(),
                0,
            )),
        ),
        (
            "bakeStrategy",
            serde_json::json!(choice_str(
                &SYSTEM_BAKE_STRATEGIES,
                window.get_bake_strategy_index(),
                0,
            )),
        ),
        (
            "onDemandPrefetchFrames",
            serde_json::json!(window.get_on_demand_prefetch_frames().clamp(0, 600)),
        ),
        (
            "enableTimelineSkimming",
            serde_json::json!(window.get_enable_timeline_skimming()),
        ),
        (
            "timelinePreviewVisible",
            serde_json::json!(window.get_timeline_preview_visible()),
        ),
        (
            "timelineTrackHeight",
            serde_json::json!(window.get_timeline_track_height().clamp(16, 100)),
        ),
        (
            "timelineHeaderHeight",
            serde_json::json!(window.get_timeline_header_height().clamp(16, 100)),
        ),
        (
            "settingDialogSidebarRight",
            serde_json::json!(window.get_setting_dialog_sidebar_right()),
        ),
        (
            "timelineRulerHeight",
            serde_json::json!(window.get_timeline_ruler_height().clamp(16, 100)),
        ),
        (
            "timelineMaxLayers",
            serde_json::json!(
                window
                    .get_timeline_max_layers()
                    .clamp(1, MAX_TIMELINE_LAYERS)
            ),
        ),
        (
            "timelineLayerHeaderWidth",
            serde_json::json!(window.get_timeline_layer_header_width().clamp(40, 300)),
        ),
        (
            "timelineClipResizeHandleWidth",
            serde_json::json!(window.get_timeline_clip_resize_handle_width().clamp(4, 40)),
        ),
        (
            "minClipDurationFrames",
            serde_json::json!(window.get_min_clip_duration_frames().clamp(1, 100)),
        ),
        ("timelineZoomMin", serde_json::json!(zoom_minimum)),
        ("timelineZoomMax", serde_json::json!(zoom_maximum)),
        (
            "timelineZoomStep",
            serde_json::json!(window.get_timeline_zoom_step().clamp(1, 100)),
        ),
        (
            "theme",
            serde_json::json!(choice_str(
                &SYSTEM_THEME_VALUES,
                window.get_theme_index(),
                0,
            )),
        ),
        (
            "uiLanguage",
            serde_json::json!(choice_str(
                &SYSTEM_UI_LANGUAGE_VALUES,
                window.get_language_index(),
                0,
            )),
        ),
        (
            "defaultProjectWidth",
            serde_json::json!(window.get_default_project_width().clamp(1, 16_000)),
        ),
        (
            "defaultProjectHeight",
            serde_json::json!(window.get_default_project_height().clamp(1, 16_000)),
        ),
        ("defaultProjectFps", serde_json::json!(fps)),
        (
            "defaultProjectFrames",
            serde_json::json!(window.get_default_project_frames().clamp(1, 1_000_000)),
        ),
        (
            "defaultProjectSampleRate",
            serde_json::json!(
                window
                    .get_default_project_sample_rate()
                    .clamp(8_000, 192_000)
            ),
        ),
        (
            "defaultClipDuration",
            serde_json::json!(window.get_default_clip_duration().clamp(1, 100_000)),
        ),
        (
            "exportDefaultCodec",
            serde_json::json!(choice_str(
                &SYSTEM_EXPORT_VIDEO_CODECS,
                window.get_export_video_codec_index(),
                2,
            )),
        ),
        (
            "exportDefaultBitrateMbps",
            serde_json::json!(window.get_export_default_bitrate_mbps().clamp(1, 500)),
        ),
        (
            "exportDefaultCrf",
            serde_json::json!(window.get_export_default_crf().clamp(0, 51)),
        ),
        (
            "exportImageQuality",
            serde_json::json!(window.get_export_image_quality().clamp(0, 100)),
        ),
        (
            "exportSequencePadding",
            serde_json::json!(window.get_export_sequence_padding().clamp(2, 10)),
        ),
        (
            "exportDefaultAudioCodec",
            serde_json::json!(choice_str(
                &SYSTEM_EXPORT_AUDIO_CODECS,
                window.get_export_audio_codec_index(),
                0,
            )),
        ),
        (
            "exportDefaultAudioBitrateKbps",
            serde_json::json!(
                window
                    .get_export_default_audio_bitrate_kbps()
                    .clamp(32, 1_536)
            ),
        ),
        (
            "exportFrameGrabTimeoutMs",
            serde_json::json!(window.get_export_frame_grab_timeout_ms().clamp(100, 10_000)),
        ),
        (
            "exportProgressInterval",
            serde_json::json!(window.get_export_progress_interval().clamp(1, 60)),
        ),
        (
            "exportEncoderQueueMB",
            serde_json::json!(window.get_export_encoder_queue_mb().clamp(16, 1_024)),
        ),
        (
            "videoDecoderIndexReserve",
            serde_json::json!(
                window
                    .get_video_decoder_index_reserve()
                    .clamp(1_000, 1_000_000)
            ),
        ),
        (
            "videoDecoderMinCacheMB",
            serde_json::json!(window.get_video_decoder_min_cache_mb().clamp(16, 4_096)),
        ),
        (
            "hwFramePoolSize",
            serde_json::json!(window.get_hw_frame_pool_size().clamp(1, 256)),
        ),
        (
            "audioPluginMaxBlockSize",
            serde_json::json!(choice_i32(
                &SYSTEM_AUDIO_BLOCK_SIZES,
                window.get_audio_plugin_block_size_index(),
                4,
            )),
        ),
        (
            "luaHookIntervalMs",
            serde_json::json!(window.get_lua_hook_interval_ms().clamp(1, 1_000)),
        ),
        (
            "luaHotReload",
            serde_json::json!(window.get_lua_hot_reload()),
        ),
    ] {
        replacement.insert(key.to_owned(), value);
    }

    let plugin_settings = window.get_plugin_settings();
    for index in 0..plugin_settings.row_count() {
        let Some(row) = plugin_settings.row_data(index) else {
            continue;
        };
        replacement.insert(
            format!("pluginEnable{}", row.format_name),
            serde_json::json!(row.enabled),
        );
        replacement.insert(
            format!("pluginPaths{}", row.format_name),
            plugin_paths_value(&row.paths),
        );
    }

    let mut shortcuts = replacement
        .get("shortcuts")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let shortcut_settings = window.get_shortcut_settings();
    crate::shortcuts::validate_shortcuts(
        &shortcut_settings
            .iter()
            .map(|row| row.value.to_string())
            .collect::<Vec<_>>(),
    )
    .map_err(str::to_owned)?;
    for index in 0..shortcut_settings.row_count() {
        let Some(row) = shortcut_settings.row_data(index) else {
            continue;
        };
        shortcuts.insert(
            row.action_id.to_string(),
            serde_json::Value::String(row.value.to_string()),
        );
    }
    replacement.insert("shortcuts".to_owned(), serde_json::Value::Object(shortcuts));
    Ok(replacement)
}

pub(super) fn choice_str<'a>(choices: &'a [&'a str], index: i32, fallback: usize) -> &'a str {
    usize::try_from(index)
        .ok()
        .and_then(|index| choices.get(index).copied())
        .unwrap_or(choices[fallback])
}

pub(super) fn choice_i32(choices: &[i32], index: i32, fallback: usize) -> i32 {
    usize::try_from(index)
        .ok()
        .and_then(|index| choices.get(index).copied())
        .unwrap_or(choices[fallback])
}

pub(super) fn choice_f64(choices: &[f64], index: i32, fallback: usize) -> f64 {
    usize::try_from(index)
        .ok()
        .and_then(|index| choices.get(index).copied())
        .unwrap_or(choices[fallback])
}

pub(super) fn plugin_paths_value(text: &str) -> serde_json::Value {
    serde_json::Value::Array(
        text.lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(|path| serde_json::Value::String(path.to_owned()))
            .collect(),
    )
}

pub(super) fn parse_required_i32(
    value: &str,
    label: &str,
    minimum: i32,
    maximum: i32,
) -> Result<i32, String> {
    let parsed = value
        .trim()
        .parse::<i32>()
        .map_err(|_| match current_ui_language() {
            UiLanguage::English => format!("Enter an integer for {label}."),
            UiLanguage::SimplifiedChinese => format!("请为{label}输入整数。"),
            UiLanguage::Japanese => format!("{label}には整数を入力してください"),
        })?;
    if (minimum..=maximum).contains(&parsed) {
        Ok(parsed)
    } else {
        Err(match current_ui_language() {
            UiLanguage::English => {
                format!("Enter {label} in the range {minimum} to {maximum}.")
            }
            UiLanguage::SimplifiedChinese => {
                format!("请在 {minimum} 到 {maximum} 的范围内输入{label}。")
            }
            UiLanguage::Japanese => {
                format!("{label}は{minimum}から{maximum}の範囲で入力してください")
            }
        })
    }
}

pub(super) fn parse_required_f64(
    value: &str,
    label: &str,
    minimum: f64,
    maximum: f64,
) -> Result<f64, String> {
    let parsed = value
        .trim()
        .parse::<f64>()
        .map_err(|_| match current_ui_language() {
            UiLanguage::English => format!("Enter a number for {label}."),
            UiLanguage::SimplifiedChinese => format!("请为{label}输入数值。"),
            UiLanguage::Japanese => format!("{label}には数値を入力してください"),
        })?;
    if parsed.is_finite() && (minimum..=maximum).contains(&parsed) {
        Ok(parsed)
    } else {
        Err(match current_ui_language() {
            UiLanguage::English => {
                format!("Enter {label} in the range {minimum} to {maximum}.")
            }
            UiLanguage::SimplifiedChinese => {
                format!("请在 {minimum} 到 {maximum} 的范围内输入{label}。")
            }
            UiLanguage::Japanese => {
                format!("{label}は{minimum}から{maximum}の範囲で入力してください")
            }
        })
    }
}

pub(super) fn parse_finite_f64(value: &str, fallback: f64) -> f64 {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .unwrap_or(fallback)
}

pub(super) fn parse_i32_unbounded(value: &str, fallback: i32) -> i32 {
    value.trim().parse::<i32>().unwrap_or(fallback)
}

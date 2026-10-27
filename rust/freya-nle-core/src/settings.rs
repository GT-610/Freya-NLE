use crate::project::DEFAULT_FPS;
use serde_json::{Map, Value, json};
use std::error::Error;
use std::fmt::{Display, Formatter};

const DEFAULT_WIDTH: i32 = 1920;
const DEFAULT_HEIGHT: i32 = 1080;
const DEFAULT_SAMPLE_RATE: i32 = 48_000;
const DEFAULT_CLIP_DURATION: i32 = 100;
const AUDIO_MAX_BLOCK_SIZE: i32 = 4096;

const PLATFORM_PATH_KEYS: [&str; 11] = [
    "pluginPathsLADSPA",
    "pluginPathsDSSI",
    "pluginPathsLV2",
    "pluginPathsVST2",
    "pluginPathsVST3",
    "pluginPathsCLAP",
    "pluginPathsSF2",
    "pluginPathsSFZ",
    "pluginPathsJSFX",
    "pluginPathsEffects",
    "pluginPathsObjects",
];

fn shortcuts() -> Value {
    json!({
        "project.new": "Ctrl+N",
        "project.save": "Ctrl+S",
        "project.open": "Ctrl+O",
        "project.saveAs": "Ctrl+Shift+S",
        "project.export": "Ctrl+E",
        "app.quit": "Ctrl+Q",
        "app.settings": "Ctrl+P",
        "edit.undo": "Ctrl+Z",
        "edit.redo": "Ctrl+Shift+Z",
        "edit.cut": "Ctrl+X",
        "edit.copy": "Ctrl+C",
        "edit.paste": "Ctrl+V",
        "edit.delete": "Delete",
        "edit.duplicate": "Ctrl+D",
        "transport.playPause": "Space",
        "transport.nextFrame": "Right",
        "transport.prevFrame": "Left",
        "transport.jumpStart": "Home",
        "transport.jumpEnd": "End",
        "view.zoomIn": "Ctrl++",
        "view.zoomOut": "Ctrl+-",
        "view.timeline": "F3",
        "view.objectSettings": "F4",
        "project.settings": "Alt+Enter",
        "timeline.split": "S",
        "timeline.moveUp": "Alt+Up",
        "timeline.moveDown": "Alt+Down",
        "timeline.nudgeLeft": "Alt+Left",
        "timeline.nudgeRight": "Alt+Right",
        "timeline.addScene": "Ctrl+T",
        "timeline.sceneSettings": "Alt+S",
        "timeline.removeScene": "Ctrl+Shift+Delete",
        "timeline.layerLock": "Ctrl+L",
        "timeline.layerHide": "Ctrl+H"
    })
}

fn default_settings(platform_defaults: &Map<String, Value>) -> Map<String, Value> {
    let mut settings = json!({
        "pluginEnableLADSPA": true,
        "pluginEnableDSSI": true,
        "pluginEnableLV2": true,
        "pluginEnableVST2": true,
        "pluginEnableVST3": true,
        "pluginEnableCLAP": true,
        "pluginEnableSF2": true,
        "pluginEnableSFZ": true,
        "pluginEnableJSFX": true,
        "pluginEnableEffects": true,
        "pluginEnableObjects": true,
        "packageRepositories": [{
            "url": "https://raw.githubusercontent.com/GT-610/AviQtl-Plus/main/repos/repo.json",
            "name": "AviQtl Official",
            "enabled": true,
            "priority": 10
        }],
        "maxImageSize": 8192,
        "cacheSize": 512,
        "undoCount": 32,
        "theme": "Dark",
        "showConfirmOnClose": true,
        "enableAutoBackup": true,
        "backupInterval": 5,
        "defaultProjectWidth": DEFAULT_WIDTH,
        "defaultProjectHeight": DEFAULT_HEIGHT,
        "defaultProjectFps": DEFAULT_FPS,
        "defaultProjectFrames": 3600,
        "defaultProjectSampleRate": DEFAULT_SAMPLE_RATE,
        "defaultClipDuration": DEFAULT_CLIP_DURATION,
        "enableSnap": true,
        "enableTimelineSkimming": true,
        "settingDialogSidebarRight": false,
        "timelineTrackHeight": 30,
        "timelineHeaderHeight": 28,
        "timelineRulerHeight": 32,
        "timelineMaxLayers": 128,
        "timelineLayerHeaderWidth": 60,
        "timelineClipResizeHandleWidth": 10,
        // Retained from the Qt frontend: its splash phase no longer exists, but a
        // settings file written by an older version keeps the key.
        "splashSize": 512,
        "exportImageQuality": 95,
        "exportSequencePadding": 6,
        "minClipDurationFrames": 5,
        "magneticSnapRange": 10,
        "timelineZoomMin": 10,
        "timelineZoomMax": 400,
        "timelineZoomStep": 10,
        "videoDecoderIndexReserve": 108000,
        "videoDecoderMinCacheMB": 64,
        "hwFramePoolSize": 32,
        "bakeStrategy": "OnDemand",
        "onDemandPrefetchFrames": 30,
        "previewRenderScale": 1.0,
        "previewMsaaSamples": 0,
        "exportDefaultCodec": "libx264",
        "exportDefaultBitrateMbps": 15,
        "exportDefaultCrf": 20,
        "exportDefaultAudioCodec": "aac",
        "exportDefaultAudioBitrateKbps": 192,
        "exportFrameGrabTimeoutMs": 2000,
        "exportProgressInterval": 5,
        "exportEncoderQueueMB": 128,
        "audioPluginMaxBlockSize": AUDIO_MAX_BLOCK_SIZE,
        "sceneWidthMax": 8000,
        "sceneHeightMax": 8000,
        "sceneFramesMin": 100,
        "sceneFramesMax": 24000,
        "sceneFramesStep": 100,
        "recentProjectMaxCount": 10,
        "luaHookIntervalMs": 16,
        "shortcuts": shortcuts()
    })
    .as_object()
    .cloned()
    .expect("settings defaults must be an object");

    for key in PLATFORM_PATH_KEYS {
        settings.insert(
            key.to_owned(),
            platform_defaults
                .get(key)
                .filter(|value| value.is_array())
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())),
        );
    }
    settings
}

fn merge_settings(
    base: &Map<String, Value>,
    loaded: &Map<String, Value>,
) -> (Map<String, Value>, bool) {
    let mut settings = base.clone();
    for (key, value) in loaded {
        if key == "shortcuts" && value.is_object() {
            let mut merged = settings
                .get("shortcuts")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            merged.extend(value.as_object().cloned().unwrap_or_default());
            settings.insert(key.clone(), Value::Object(merged));
        } else {
            settings.insert(key.clone(), value.clone());
        }
    }

    let migrated = loaded.contains_key("packageRepositoryUrls");
    if let Some(legacy) = loaded.get("packageRepositoryUrls") {
        if !loaded.contains_key("packageRepositories") {
            let repositories: Vec<Value> = legacy
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|url| !url.is_empty())
                .map(|url| {
                    json!({
                        "url": url,
                        "name": url,
                        "enabled": true,
                        "priority": 10
                    })
                })
                .collect();
            if !repositories.is_empty() {
                settings.insert("packageRepositories".to_owned(), Value::Array(repositories));
            }
        }
        settings.remove("packageRepositoryUrls");
    }
    (settings, migrated)
}

fn persistent_settings(settings: &Map<String, Value>) -> Map<String, Value> {
    settings
        .iter()
        .filter(|(key, _)| !key.starts_with('_'))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn parse_object(input: &[u8]) -> Option<Map<String, Value>> {
    serde_json::from_slice::<Value>(input)
        .ok()?
        .as_object()
        .cloned()
}

fn value_as_i32(value: &Value) -> i32 {
    match value {
        Value::Bool(value) => i32::from(*value),
        Value::Number(value) => value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .or_else(|| value.as_u64().and_then(|value| i32::try_from(value).ok()))
            .or_else(|| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(|value| value as i32)
            })
            .unwrap_or(0),
        Value::String(value) => value.parse().unwrap_or(0),
        _ => 0,
    }
}

fn value_as_f64(value: &Value) -> f64 {
    match value {
        Value::Bool(value) => f64::from(u8::from(*value)),
        Value::Number(value) => value.as_f64().unwrap_or(0.0),
        Value::String(value) => value.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn value_as_bool(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError;

impl Display for SettingsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("invalid settings document")
    }
}

impl Error for SettingsError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsMutation {
    pub changed: bool,
    pub persistent: bool,
}

#[derive(Debug, Clone)]
pub struct SettingsState {
    settings: Map<String, Value>,
}

impl SettingsState {
    pub fn defaults(platform_defaults: Map<String, Value>) -> Self {
        Self {
            settings: default_settings(&platform_defaults),
        }
    }

    pub fn merge_json(&mut self, input: &[u8]) -> Result<bool, SettingsError> {
        let loaded = parse_object(input).ok_or(SettingsError)?;
        let (settings, migrated) = merge_settings(&self.settings, &loaded);
        self.settings = settings;
        Ok(migrated)
    }

    pub fn replace(&mut self, settings: Map<String, Value>) {
        self.settings = settings;
    }

    pub fn snapshot(&self) -> Map<String, Value> {
        self.settings.clone()
    }

    pub fn persistent_snapshot(&self) -> Map<String, Value> {
        persistent_settings(&self.settings)
    }

    pub fn persistent_json(&self) -> Result<Vec<u8>, SettingsError> {
        serde_json::to_vec_pretty(&self.persistent_snapshot()).map_err(|_| SettingsError)
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.settings.get(key)
    }

    pub fn i32_value(&self, key: &str, fallback: i32) -> i32 {
        self.settings.get(key).map(value_as_i32).unwrap_or(fallback)
    }

    pub fn f64_value(&self, key: &str, fallback: f64) -> f64 {
        self.settings.get(key).map(value_as_f64).unwrap_or(fallback)
    }

    pub fn bool_value(&self, key: &str, fallback: bool) -> bool {
        self.settings
            .get(key)
            .map(value_as_bool)
            .unwrap_or(fallback)
    }

    pub fn set_value(&mut self, key: impl Into<String>, value: Value) -> SettingsMutation {
        let key = key.into();
        let changed = self.settings.get(&key) != Some(&value);
        if changed {
            self.settings.insert(key.clone(), value);
        }
        SettingsMutation {
            changed,
            persistent: !key.starts_with('_'),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_own_schema_and_accept_only_platform_path_arrays() {
        let defaults = default_settings(
            json!({
                "pluginPathsVST3": ["/platform/vst3"],
                "theme": "untrusted-platform-value",
                "pluginPathsLV2": "not-an-array"
            })
            .as_object()
            .expect("fixture object"),
        );
        assert_eq!(defaults.get("theme"), Some(&json!("Dark")));
        assert_eq!(
            defaults.get("pluginPathsVST3"),
            Some(&json!(["/platform/vst3"]))
        );
        assert_eq!(defaults.get("pluginPathsLV2"), Some(&json!([])));
        assert_eq!(
            defaults
                .get("shortcuts")
                .and_then(Value::as_object)
                .and_then(|shortcuts| shortcuts.get("project.save")),
            Some(&json!("Ctrl+S"))
        );
    }

    #[test]
    fn merge_preserves_defaults_and_migrates_legacy_repositories() {
        let base = default_settings(&Map::new());
        let loaded = json!({
            "theme": "Light",
            "shortcuts": {"project.save": "Meta+S"},
            "packageRepositoryUrls": ["https://example.invalid/repo.json"]
        });
        let (merged, migrated) = merge_settings(&base, loaded.as_object().expect("fixture object"));
        assert!(migrated);
        assert_eq!(merged.get("theme"), Some(&json!("Light")));
        assert_eq!(
            merged
                .get("shortcuts")
                .and_then(Value::as_object)
                .and_then(|shortcuts| shortcuts.get("project.new")),
            Some(&json!("Ctrl+N"))
        );
        assert_eq!(
            merged
                .get("shortcuts")
                .and_then(Value::as_object)
                .and_then(|shortcuts| shortcuts.get("project.save")),
            Some(&json!("Meta+S"))
        );
        assert!(!merged.contains_key("packageRepositoryUrls"));
        assert_eq!(
            merged
                .get("packageRepositories")
                .and_then(Value::as_array)
                .and_then(|repositories| repositories.first())
                .and_then(Value::as_object)
                .and_then(|repository| repository.get("name")),
            Some(&json!("https://example.invalid/repo.json"))
        );

        for invalid in [json!("not-an-array"), json!([]), json!([""])] {
            let loaded = json!({"packageRepositoryUrls": invalid});
            let (merged, migrated) =
                merge_settings(&base, loaded.as_object().expect("fixture object"));
            assert!(migrated);
            assert_eq!(
                merged.get("packageRepositories"),
                base.get("packageRepositories")
            );
            assert!(!merged.contains_key("packageRepositoryUrls"));
        }
    }

    #[test]
    fn defaults_match_the_documented_project_baseline() {
        // AviUtl ExEdit's defaults, carried over from the Qt frontend's constants header.
        assert_eq!(DEFAULT_WIDTH, 1_920);
        assert_eq!(DEFAULT_HEIGHT, 1_080);
        assert_eq!(DEFAULT_SAMPLE_RATE, 48_000);
        assert_eq!(DEFAULT_CLIP_DURATION, 100);
        assert_eq!(AUDIO_MAX_BLOCK_SIZE, 4_096);
        assert_eq!(DEFAULT_FPS, 60.0);
    }

    #[test]
    fn persistent_document_removes_only_runtime_keys() {
        let settings = json!({
            "theme": "Dark",
            "_runtime": 1,
            "nested": {"_preserved": true}
        });
        let persistent = persistent_settings(settings.as_object().expect("fixture object"));
        assert!(!persistent.contains_key("_runtime"));
        assert_eq!(persistent.get("nested"), Some(&json!({"_preserved": true})));
    }

    #[test]
    fn typed_value_coercion_matches_runtime_settings_expectations() {
        for (value, expected) in [
            (json!(42), 42),
            (json!(42.9), 42),
            (json!(true), 1),
            (json!("17"), 17),
            (json!("invalid"), 0),
        ] {
            assert_eq!(value_as_i32(&value), expected);
        }
        assert_eq!(value_as_f64(&json!(1.25)), 1.25);
        assert_eq!(value_as_f64(&json!("2.5")), 2.5);
        assert_eq!(value_as_f64(&json!("invalid")), 0.0);
        assert!(!value_as_bool(&json!(false)));
        assert!(!value_as_bool(&json!(0)));
        assert!(!value_as_bool(&json!("false")));
        assert!(value_as_bool(&json!("enabled")));
    }

    fn settings_from_json(input: &[u8]) -> SettingsState {
        let mut state = SettingsState::defaults(Map::new());
        state.merge_json(input).expect("valid settings document");
        state
    }

    #[test]
    fn settings_state_owns_mutation_merge_typed_reads_and_persistence() {
        let mut state = settings_from_json(br#"{"theme":"Dark","_runtime":1,"count":3}"#);
        assert_eq!(state.i32_value("count", -1), 3);
        assert_eq!(state.i32_value("missing", 9), 9);

        let mutation = state.set_value("answer", json!(42));
        assert!(mutation.changed);
        assert!(mutation.persistent);
        // An identical value is a semantic no-op.
        assert!(!state.set_value("answer", json!(42)).changed);
        let runtime = state.set_value("_temporary", json!(true));
        assert!(runtime.changed);
        assert!(!runtime.persistent);

        let migrated = state
            .merge_json(br#"{"theme":"Light","packageRepositoryUrls":["https://example.invalid"]}"#)
            .expect("valid merge");
        assert!(migrated);
        let complete = state.snapshot();
        assert_eq!(complete.get("theme"), Some(&json!("Light")));
        assert_eq!(complete.get("answer"), Some(&json!(42)));
        assert!(!complete.contains_key("packageRepositoryUrls"));
        let persisted = state.persistent_snapshot();
        assert!(!persisted.contains_key("_temporary"));
        assert_eq!(persisted.get("answer"), Some(&json!(42)));
    }

    #[test]
    fn settings_state_default_creation_owns_the_complete_schema() {
        let platform = json!({"pluginPathsVST3":["/platform/vst3"],"theme":"Light"})
            .as_object()
            .cloned()
            .expect("fixture object");
        let state = SettingsState::defaults(platform);
        let defaults = state.snapshot();
        assert_eq!(defaults.get("theme"), Some(&json!("Dark")));
        assert_eq!(
            defaults.get("pluginPathsVST3"),
            Some(&json!(["/platform/vst3"]))
        );
        assert_eq!(defaults.get("defaultProjectWidth"), Some(&json!(1920)));
    }

    #[test]
    fn settings_state_rejects_invalid_updates_without_partial_state() {
        let mut state = settings_from_json(br#"{"value":1}"#);
        let before = state.snapshot();
        assert!(state.merge_json(b"not-json").is_err());
        assert!(state.merge_json(b"[1]").is_err());
        assert_eq!(state.snapshot(), before);
        assert_eq!(state.i32_value("value", 0), 1);
    }
}

use crate::package::compare_versions;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

const MANIFEST_ID_LIMIT: usize = 255;
const MANIFEST_NAME_LIMIT: usize = 255;
const MANIFEST_VERSION_LIMIT: usize = 64;
const MANIFEST_MIN_APP_VERSION_LIMIT: usize = 64;

const MANIFEST_FIELDS: [&str; 6] = [
    "id",
    "name",
    "version",
    "author",
    "description",
    "minAppVersion",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioPluginInfo {
    pub id: String,
    pub name: String,
    pub format: String,
    pub category: String,
    pub path: String,
    pub label: String,
    pub maker: String,
    pub unique_id: i64,
    pub index: i32,
    pub audio_ins: i32,
    pub audio_outs: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptPluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub min_app_version: String,
}

impl ScriptPluginManifest {
    fn from_map(manifest: Map<String, Value>) -> Self {
        Self {
            id: text(manifest.get("id")),
            name: text(manifest.get("name")),
            version: text(manifest.get("version")),
            author: text(manifest.get("author")),
            description: text(manifest.get("description")),
            min_app_version: text(manifest.get("minAppVersion")),
        }
    }

    fn to_map(&self) -> Map<String, Value> {
        json!({
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "author": self.author,
            "description": self.description,
            "minAppVersion": self.min_app_version,
        })
        .as_object()
        .cloned()
        .expect("script manifest projection must be an object")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptPluginValidationStatus {
    Ok,
    InvalidManifest,
    InvalidId,
    RequiresNewerApp,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptPluginIdentity {
    pub id: String,
    pub path: String,
}

/// Parses the declarative subset accepted for `manifest.lua` without executing Lua code.
pub fn parse_script_plugin_manifest(input: &str) -> Option<ScriptPluginManifest> {
    let mut parser = ManifestParser::new(input);
    let manifest = parser.parse()?;
    Some(ScriptPluginManifest::from_map(normalize_manifest(manifest)))
}

/// Applies the same identity, version, and duplicate policy used by the Qt plugin adapter.
pub fn validate_script_plugin_manifest(
    manifest: ScriptPluginManifest,
    single_file: bool,
    expected_id: &str,
    app_version: &str,
    path_identity: &str,
    loaded: &[ScriptPluginIdentity],
) -> (ScriptPluginManifest, ScriptPluginValidationStatus) {
    let loaded = loaded
        .iter()
        .map(|plugin| {
            json!({"id": plugin.id, "path": plugin.path})
                .as_object()
                .cloned()
                .expect("script identity projection must be an object")
        })
        .collect::<Vec<_>>();
    let (manifest, status) = validate_manifest(
        manifest.to_map(),
        single_file,
        expected_id,
        app_version,
        path_identity,
        &loaded,
    );
    let status = match status {
        "ok" => ScriptPluginValidationStatus::Ok,
        "invalid_manifest" => ScriptPluginValidationStatus::InvalidManifest,
        "invalid_id" => ScriptPluginValidationStatus::InvalidId,
        "requires_newer_app" => ScriptPluginValidationStatus::RequiresNewerApp,
        "duplicate" => ScriptPluginValidationStatus::Duplicate,
        _ => ScriptPluginValidationStatus::InvalidManifest,
    };
    (ScriptPluginManifest::from_map(manifest), status)
}

struct ManifestParser<'a> {
    input: &'a [u8],
    index: usize,
}

impl<'a> ManifestParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            index: usize::from(input.as_bytes().starts_with(&[0xef, 0xbb, 0xbf])) * 3,
        }
    }

    fn parse(&mut self) -> Option<Map<String, Value>> {
        self.skip_layout();
        self.consume_keyword(b"return")?;
        self.skip_layout();
        self.consume(b'{')?;
        let mut manifest = Map::new();
        loop {
            self.skip_layout();
            if self.consume_if(b'}') {
                break;
            }
            let key = self.identifier()?;
            self.skip_layout();
            self.consume(b'=')?;
            self.skip_layout();
            let value = self.string()?;
            let key = match key.as_str() {
                "min_app_version" => "minAppVersion",
                other => other,
            };
            manifest.insert(key.to_owned(), Value::String(value));
            self.skip_layout();
            if self.consume_if(b',') || self.consume_if(b';') {
                continue;
            }
            if self.peek() != Some(b'}') {
                return None;
            }
        }
        self.skip_layout();
        (self.index == self.input.len()).then_some(manifest)
    }

    fn skip_layout(&mut self) {
        loop {
            while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
                self.index += 1;
            }
            if !self.remaining().starts_with(b"--") {
                break;
            }
            self.index += 2;
            if self.remaining().starts_with(b"[[") {
                self.index += 2;
                if let Some(end) = find_bytes(self.remaining(), b"]]") {
                    self.index += end + 2;
                } else {
                    self.index = self.input.len();
                }
            } else {
                while self.peek().is_some_and(|byte| byte != b'\n') {
                    self.index += 1;
                }
            }
        }
    }

    fn consume_keyword(&mut self, keyword: &[u8]) -> Option<()> {
        if !self.remaining().starts_with(keyword) {
            return None;
        }
        self.index += keyword.len();
        if self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return None;
        }
        Some(())
    }

    fn identifier(&mut self) -> Option<String> {
        let start = self.index;
        if !self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        {
            return None;
        }
        self.index += 1;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            self.index += 1;
        }
        String::from_utf8(self.input[start..self.index].to_vec()).ok()
    }

    fn string(&mut self) -> Option<String> {
        let quote = self.peek().filter(|quote| matches!(quote, b'\'' | b'"'))?;
        self.index += 1;
        let mut value = Vec::new();
        loop {
            let byte = self.peek()?;
            self.index += 1;
            if byte == quote {
                return String::from_utf8(value).ok();
            }
            if matches!(byte, b'\n' | b'\r') {
                return None;
            }
            if byte != b'\\' {
                value.push(byte);
                continue;
            }
            let escaped = self.peek()?;
            self.index += 1;
            value.push(match escaped {
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'\\' => b'\\',
                b'\'' => b'\'',
                b'"' => b'"',
                other => other,
            });
        }
    }

    fn consume(&mut self, byte: u8) -> Option<()> {
        self.consume_if(byte).then_some(())
    }

    fn consume_if(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.index).copied()
    }

    fn remaining(&self) -> &[u8] {
        &self.input[self.index..]
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

impl AudioPluginInfo {
    fn from_map(plugin: Map<String, Value>) -> Self {
        Self {
            id: text(plugin.get("id")),
            name: text(plugin.get("name")),
            format: text(plugin.get("format")),
            category: normalize_audio_plugin_category(&text(plugin.get("category"))),
            path: text(plugin.get("path")),
            label: text(plugin.get("label")),
            maker: text(plugin.get("maker")),
            unique_id: integer(plugin.get("uniqueId")),
            index: i32::try_from(integer(plugin.get("index"))).unwrap_or_default(),
            audio_ins: i32::try_from(integer(plugin.get("audioIns"))).unwrap_or_default(),
            audio_outs: i32::try_from(integer(plugin.get("audioOuts"))).unwrap_or_default(),
        }
    }
}

/// Parses one Carla discovery transcript into normalized, typed plugin records.
pub fn parse_audio_plugin_discovery_output(
    output: &str,
    format: &str,
    file_path: &str,
    fallback_name: &str,
) -> Vec<AudioPluginInfo> {
    parse_discovery_output(output, format, file_path, fallback_name)
        .into_iter()
        .map(AudioPluginInfo::from_map)
        .collect()
}

/// Removes repeated plugin identities while preserving discovery order.
pub fn deduplicate_audio_plugins(plugins: Vec<AudioPluginInfo>) -> Vec<AudioPluginInfo> {
    let mut ids = BTreeSet::new();
    plugins
        .into_iter()
        .filter(|plugin| !plugin.id.is_empty() && ids.insert(plugin.id.clone()))
        .collect()
}

/// Lists normalized plugin categories in the same stable order as the Qt adapter.
pub fn audio_plugin_categories(plugins: &[AudioPluginInfo]) -> Vec<String> {
    let mut categories = plugins
        .iter()
        .map(|plugin| normalize_audio_plugin_category(&plugin.category))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    categories.sort_by(|left, right| {
        category_rank(left)
            .cmp(&category_rank(right))
            .then_with(|| left.to_lowercase().cmp(&right.to_lowercase()))
    });
    categories
}

/// Returns one category sorted by display name without exposing the legacy JSON projection.
pub fn audio_plugins_in_category(
    plugins: &[AudioPluginInfo],
    category: &str,
) -> Vec<AudioPluginInfo> {
    let wanted = normalize_audio_plugin_category(category);
    let mut filtered = plugins
        .iter()
        .filter(|plugin| normalize_audio_plugin_category(&plugin.category) == wanted)
        .cloned()
        .collect::<Vec<_>>();
    filtered.sort_by_key(|plugin| plugin.name.to_lowercase());
    filtered
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn integer(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_i64).unwrap_or_default()
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn normalize_manifest(mut manifest: Map<String, Value>) -> Map<String, Value> {
    for field in MANIFEST_FIELDS {
        manifest.insert(
            field.to_owned(),
            Value::String(text(manifest.get(field)).trim().to_owned()),
        );
    }
    manifest
}

fn manifest_shape_is_valid(manifest: &Map<String, Value>) -> bool {
    let id = text(manifest.get("id"));
    let name = text(manifest.get("name"));
    let version = text(manifest.get("version"));
    let minimum_app_version = text(manifest.get("minAppVersion"));
    !id.is_empty()
        && !name.is_empty()
        && !version.is_empty()
        && utf16_len(&id) <= MANIFEST_ID_LIMIT
        && utf16_len(&name) <= MANIFEST_NAME_LIMIT
        && utf16_len(&version) <= MANIFEST_VERSION_LIMIT
        && utf16_len(&minimum_app_version) <= MANIFEST_MIN_APP_VERSION_LIMIT
}

fn validate_manifest(
    manifest: Map<String, Value>,
    single_file: bool,
    expected_id: &str,
    app_version: &str,
    path_identity: &str,
    loaded: &[Map<String, Value>],
) -> (Map<String, Value>, &'static str) {
    let manifest = normalize_manifest(manifest);
    if !manifest_shape_is_valid(&manifest) {
        return (manifest, "invalid_manifest");
    }
    let id = text(manifest.get("id"));
    let valid_identity = if single_file {
        !expected_id.is_empty() && id == expected_id
    } else {
        crate::policy::valid_package_id(&id) && !id.starts_with("file:")
    };
    if !valid_identity {
        return (manifest, "invalid_id");
    }
    let minimum_app_version = text(manifest.get("minAppVersion"));
    if !minimum_app_version.is_empty() && compare_versions(app_version, &minimum_app_version) < 0 {
        return (manifest, "requires_newer_app");
    }
    if loaded.iter().any(|plugin| {
        text(plugin.get("id")) == id
            || (!path_identity.is_empty() && text(plugin.get("path")) == path_identity)
    }) {
        return (manifest, "duplicate");
    }
    (manifest, "ok")
}

/// Normalizes an audio-plugin category for stable Qt-compatible menu grouping.
pub fn normalize_audio_plugin_category(category: &str) -> String {
    let category = category.trim();
    let lower = category.to_lowercase();
    match lower.as_str() {
        "synth" | "instrument" => "Synth".to_owned(),
        "delay" | "reverb" => "Delay".to_owned(),
        "eq" => "EQ".to_owned(),
        "filter" => "Filter".to_owned(),
        "distortion" => "Distortion".to_owned(),
        "dynamics" => "Dynamics".to_owned(),
        "modulator" | "modulation" => "Modulator".to_owned(),
        "utility" | "tools" | "tool" => "Utility".to_owned(),
        "" | "other" | "unknown" | "misc" | "none" | "null" => "Other".to_owned(),
        _ => {
            let mut characters = lower.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                None => "Other".to_owned(),
            }
        }
    }
}

fn carla_category(category: i32) -> &'static str {
    match category {
        1 => "Synth",
        2 => "Delay",
        3 => "EQ",
        4 => "Filter",
        5 => "Distortion",
        6 => "Dynamics",
        7 => "Modulator",
        8 => "Utility",
        _ => "Other",
    }
}

fn category_rank(category: &str) -> i32 {
    match normalize_audio_plugin_category(category).as_str() {
        "Filter" => 0,
        "EQ" => 1,
        "Dynamics" => 2,
        "Delay" => 3,
        "Distortion" => 4,
        "Modulator" => 5,
        "Utility" => 6,
        "Synth" => 7,
        _ => 100,
    }
}

fn normalize_plugin(mut plugin: Map<String, Value>, fallback_name: &str) -> Map<String, Value> {
    let label = text(plugin.get("label")).trim().to_owned();
    let mut name = text(plugin.get("name")).trim().to_owned();
    if name.is_empty() {
        name = if label.is_empty() {
            fallback_name.trim().to_owned()
        } else {
            label.clone()
        };
    }
    let label = if label.is_empty() {
        name.clone()
    } else {
        label
    };
    let category = normalize_audio_plugin_category(&text(plugin.get("category")));
    plugin.insert("name".to_owned(), Value::String(name));
    plugin.insert("label".to_owned(), Value::String(label));
    plugin.insert("category".to_owned(), Value::String(category));
    plugin
}

fn finish_discovery_plugin(
    mut plugin: Map<String, Value>,
    fallback_name: &str,
) -> Option<Map<String, Value>> {
    plugin = normalize_plugin(plugin, fallback_name);
    let name = text(plugin.get("name"));
    if name.is_empty() {
        return None;
    }
    let format = text(plugin.get("format"));
    let label = text(plugin.get("label"));
    let unique_id = integer(plugin.get("uniqueId"));
    plugin.insert(
        "id".to_owned(),
        Value::String(format!("{format}:{label}:{unique_id}")),
    );
    Some(plugin)
}

fn parse_discovery_output(
    output: &str,
    format: &str,
    file_path: &str,
    fallback_name: &str,
) -> Vec<Map<String, Value>> {
    let mut plugins = Vec::new();
    let mut current = Map::new();
    let mut in_block = false;
    for raw_line in output.lines() {
        let line = raw_line.trim();
        let Some(payload) = line.strip_prefix("carla-discovery::") else {
            continue;
        };
        let mut parts = payload.split("::");
        let key = parts.next().unwrap_or_default();
        let value = parts.collect::<Vec<_>>().join("::");
        match key {
            "begin" | "init" => {
                current = Map::new();
                current.insert("format".to_owned(), Value::String(format.to_owned()));
                current.insert("path".to_owned(), Value::String(file_path.to_owned()));
                current.insert("uniqueId".to_owned(), Value::from(0));
                current.insert("index".to_owned(), Value::from(0));
                current.insert("audioIns".to_owned(), Value::from(0));
                current.insert("audioOuts".to_owned(), Value::from(0));
                in_block = true;
            }
            _ if !in_block => {}
            "name" | "label" | "maker" => {
                current.insert(key.to_owned(), Value::String(value));
            }
            "uniqueId" => {
                current.insert(
                    "uniqueId".to_owned(),
                    Value::from(value.trim().parse::<i64>().unwrap_or_default()),
                );
            }
            "category" => {
                let trimmed = value.trim();
                let category = trimmed
                    .parse::<i32>()
                    .ok()
                    .map(carla_category)
                    .unwrap_or(trimmed);
                current.insert(
                    "category".to_owned(),
                    Value::String(normalize_audio_plugin_category(category)),
                );
            }
            "audio.ins" => {
                current.insert(
                    "audioIns".to_owned(),
                    Value::from(value.trim().parse::<i32>().unwrap_or_default()),
                );
            }
            "audio.outs" => {
                current.insert(
                    "audioOuts".to_owned(),
                    Value::from(value.trim().parse::<i32>().unwrap_or_default()),
                );
            }
            "end" => {
                if let Some(plugin) = finish_discovery_plugin(current, fallback_name) {
                    plugins.push(plugin);
                }
                current = Map::new();
                in_block = false;
            }
            _ => {}
        }
    }
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rules_normalize_fields_and_match_qstring_limits() {
        let manifest = normalize_manifest(
            json!({
                "id": "  org.aviqtl.example  ",
                "name": " Example ",
                "version": " 1.2.3 ",
                "author": " Author ",
                "description": " Description ",
                "minAppVersion": " 0.5.0 "
            })
            .as_object()
            .expect("fixture")
            .clone(),
        );
        assert_eq!(text(manifest.get("id")), "org.aviqtl.example");
        assert_eq!(text(manifest.get("name")), "Example");
        assert_eq!(text(manifest.get("author")), "Author");
        assert!(manifest_shape_is_valid(&manifest));

        let mut too_long = manifest.clone();
        too_long.insert("name".to_owned(), Value::String("😀".repeat(128)));
        assert!(!manifest_shape_is_valid(&too_long));
    }

    #[test]
    fn manifest_rules_own_identity_compatibility_and_duplicate_checks() {
        let manifest = json!({
            "id": "org.aviqtl.example",
            "name": "Example",
            "version": "1.0.0",
            "minAppVersion": "0.6.0"
        })
        .as_object()
        .expect("fixture")
        .clone();
        assert_eq!(
            validate_manifest(
                manifest.clone(),
                false,
                "",
                "0.5.9",
                "/plugin/main.lua",
                &[]
            )
            .1,
            "requires_newer_app"
        );
        assert_eq!(
            validate_manifest(
                manifest.clone(),
                false,
                "",
                "0.6.0",
                "/plugin/main.lua",
                &[]
            )
            .1,
            "ok"
        );
        assert_eq!(
            validate_manifest(
                manifest.clone(),
                false,
                "",
                "0.6.0",
                "/plugin/main.lua",
                &[
                    json!({"id": "org.aviqtl.example", "path": "/other/main.lua"})
                        .as_object()
                        .expect("fixture")
                        .clone()
                ],
            )
            .1,
            "duplicate"
        );

        let file_manifest = json!({
            "id": "file:example.lua",
            "name": "example",
            "version": "file"
        })
        .as_object()
        .expect("fixture")
        .clone();
        assert_eq!(
            validate_manifest(
                file_manifest.clone(),
                true,
                "file:example.lua",
                "0.6.0",
                "/plugin/example.lua",
                &[],
            )
            .1,
            "ok"
        );
        assert_eq!(
            validate_manifest(
                file_manifest,
                true,
                "file:renamed.lua",
                "0.6.0",
                "/plugin/example.lua",
                &[],
            )
            .1,
            "invalid_id"
        );
    }

    #[test]
    fn discovery_parser_normalizes_complete_plugin_blocks() {
        let output = "ignored\n\
carla-discovery::init\n\
carla-discovery::name::  \n\
carla-discovery::label::Example::Stereo\n\
carla-discovery::maker::AviQtl\n\
carla-discovery::uniqueId::42\n\
carla-discovery::category::4\n\
carla-discovery::audio.ins::2\n\
carla-discovery::audio.outs::2\n\
carla-discovery::end";
        let plugins = parse_discovery_output(output, "VST3", "/tmp/example.vst3", "example");
        assert_eq!(plugins.len(), 1);
        assert_eq!(text(plugins[0].get("name")), "Example::Stereo");
        assert_eq!(text(plugins[0].get("category")), "Filter");
        assert_eq!(text(plugins[0].get("id")), "VST3:Example::Stereo:42");
        assert_eq!(integer(plugins[0].get("audioIns")), 2);

        let uncategorized = parse_discovery_output(
            "carla-discovery::init\ncarla-discovery::name::No Category\ncarla-discovery::category::0\ncarla-discovery::end",
            "VST2",
            "/tmp/no-category.so",
            "no-category",
        );
        assert_eq!(text(uncategorized[0].get("category")), "Other");
    }

    fn audio_plugin(id: &str, name: &str, category: &str, format: &str) -> AudioPluginInfo {
        AudioPluginInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            format: format.to_owned(),
            category: category.to_owned(),
            path: String::new(),
            label: String::new(),
            maker: String::new(),
            unique_id: 0,
            index: 0,
            audio_ins: 0,
            audio_outs: 0,
        }
    }

    #[test]
    fn catalog_deduplication_categories_and_filtering_are_stable() {
        let plugins = vec![
            audio_plugin("first", "Zulu", "reverb", "LV2"),
            audio_plugin("second", "Alpha", "delay", "VST3"),
            audio_plugin("first", "Duplicate", "filter", "VST2"),
            audio_plugin("third", "EQ", "eq", "LV2"),
            audio_plugin("fourth", "Custom One", "Custom", "LV2"),
            audio_plugin("fifth", "Custom Two", "custom", "LV2"),
        ];
        let plugins = deduplicate_audio_plugins(plugins);
        assert_eq!(plugins.len(), 5);
        assert_eq!(
            audio_plugin_categories(&plugins),
            vec!["EQ", "Delay", "Custom"]
        );
        let filtered = audio_plugins_in_category(&plugins, "Delay");
        assert_eq!(
            filtered
                .iter()
                .map(|plugin| plugin.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Zulu"]
        );
        assert_eq!(audio_plugins_in_category(&plugins, "CUSTOM").len(), 2);
    }
}

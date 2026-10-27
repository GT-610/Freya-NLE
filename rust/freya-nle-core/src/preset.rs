use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

const PRESET_VERSION: i32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectPreset {
    pub version: i32,
    #[serde(rename = "effectId")]
    pub effect_id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub params: Map<String, Value>,
    #[serde(default)]
    pub keyframes: Map<String, Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetError {
    InvalidName,
    InvalidDocument,
    Serialization,
}

impl Display for PresetError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidName => "preset effect and preset names must be safe path components",
            Self::InvalidDocument => "preset document is invalid or does not match its identity",
            Self::Serialization => "preset document could not be serialized",
        })
    }
}

impl Error for PresetError {}

pub fn preset_name_is_safe(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && !value.contains("..")
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}

fn build_document(
    effect_id: &str,
    name: &str,
    enabled: bool,
    params: Map<String, Value>,
    keyframes: Map<String, Value>,
) -> Option<EffectPreset> {
    if !preset_name_is_safe(effect_id) || !preset_name_is_safe(name) {
        return None;
    }
    Some(EffectPreset {
        version: PRESET_VERSION,
        effect_id: effect_id.to_owned(),
        name: name.to_owned(),
        enabled,
        params,
        keyframes,
        extra: BTreeMap::new(),
    })
}

fn normalize_document(input: &[u8], effect_id: &str, name: &str) -> Option<EffectPreset> {
    if !preset_name_is_safe(effect_id) || !preset_name_is_safe(name) {
        return None;
    }
    let document: EffectPreset = serde_json::from_slice(input).ok()?;
    if document.version != PRESET_VERSION
        || document.effect_id != effect_id
        || document.name != name
        || !preset_name_is_safe(&document.effect_id)
        || !preset_name_is_safe(&document.name)
    {
        return None;
    }
    Some(document)
}

pub fn build_effect_preset(
    effect_id: &str,
    name: &str,
    enabled: bool,
    params: Map<String, Value>,
    keyframes: Map<String, Value>,
) -> Result<Vec<u8>, PresetError> {
    let document = build_document(effect_id, name, enabled, params, keyframes)
        .ok_or(PresetError::InvalidName)?;
    serde_json::to_vec(&document).map_err(|_| PresetError::Serialization)
}

pub fn parse_effect_preset(
    input: &[u8],
    effect_id: &str,
    name: &str,
) -> Result<EffectPreset, PresetError> {
    if !preset_name_is_safe(effect_id) || !preset_name_is_safe(name) {
        return Err(PresetError::InvalidName);
    }
    normalize_document(input, effect_id, name).ok_or(PresetError::InvalidDocument)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_reject_hidden_and_traversal_components() {
        assert!(preset_name_is_safe("Warm Look"));
        assert!(!preset_name_is_safe(""));
        assert!(!preset_name_is_safe(".hidden"));
        assert!(!preset_name_is_safe("../escape"));
        assert!(!preset_name_is_safe("nested/name"));
        assert!(!preset_name_is_safe("nested\\name"));
    }

    #[test]
    fn document_build_and_normalization_enforce_identity_and_schema() {
        let document = build_document(
            "effect.test",
            "Warm Look",
            true,
            json!({"amount": 2}).as_object().cloned().expect("fixture"),
            Map::new(),
        )
        .expect("valid document");
        let json = serde_json::to_vec(&document).expect("serialize fixture");
        assert_eq!(
            normalize_document(&json, "effect.test", "Warm Look"),
            Some(document)
        );
        assert!(normalize_document(&json, "effect.other", "Warm Look").is_none());

        let manually_edited = json!({
            "version": 1,
            "effectId": "effect.test",
            "name": "Warm Look"
        });
        let normalized = normalize_document(
            &serde_json::to_vec(&manually_edited).expect("serialize fixture"),
            "effect.test",
            "Warm Look",
        )
        .expect("omitted fields use defaults");
        assert!(!normalized.enabled);
        assert!(normalized.params.is_empty());
        assert!(normalized.keyframes.is_empty());

        let unsupported = json!({
            "version": 2,
            "effectId": "effect.test",
            "name": "Warm Look",
            "enabled": true,
            "params": {},
            "keyframes": {}
        });
        assert!(
            normalize_document(
                &serde_json::to_vec(&unsupported).expect("serialize fixture"),
                "effect.test",
                "Warm Look"
            )
            .is_none()
        );
    }
}

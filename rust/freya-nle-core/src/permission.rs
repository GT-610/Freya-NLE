use crate::policy::{PERMISSION_NAMES, permission_for_api, permission_from_name};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum PluginPermission {
    TransportControl,
    ClipRead,
    ClipModify,
    EffectModify,
    ProjectRead,
    ProjectSave,
    ProjectLoad,
    SceneManage,
    SettingsRead,
    SettingsWrite,
    ClipboardAccess,
    HistoryControl,
    LogOutput,
}

impl PluginPermission {
    pub const ALL: [Self; 13] = [
        Self::TransportControl,
        Self::ClipRead,
        Self::ClipModify,
        Self::EffectModify,
        Self::ProjectRead,
        Self::ProjectSave,
        Self::ProjectLoad,
        Self::SceneManage,
        Self::SettingsRead,
        Self::SettingsWrite,
        Self::ClipboardAccess,
        Self::HistoryControl,
        Self::LogOutput,
    ];

    pub fn name(self) -> &'static str {
        PERMISSION_NAMES[self as usize]
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .get(usize::try_from(permission_from_name(name)).ok()?)
            .copied()
    }

    pub fn for_api(api_name: &str) -> Option<Self> {
        Self::ALL
            .get(usize::try_from(permission_for_api(api_name)).ok()?)
            .copied()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PermissionState {
    plugins: BTreeMap<String, u64>,
}

fn permission_is_valid(permission: i32) -> bool {
    usize::try_from(permission).is_ok_and(|permission| permission < PERMISSION_NAMES.len())
}

impl PermissionState {
    #[cfg(test)]
    fn from_json(input: &[u8]) -> Option<Self> {
        let root = serde_json::from_slice::<Value>(input).ok()?;
        Self::from_value(&root)
    }

    fn from_value(root: &Value) -> Option<Self> {
        let object = root.as_object()?;
        let mut plugins = BTreeMap::new();
        for (plugin_id, permissions) in object {
            let mut mask = 0_u64;
            if let Some(names) = permissions.as_array() {
                for name in names.iter().filter_map(Value::as_str) {
                    let permission = permission_from_name(name);
                    if permission >= 0 {
                        mask |= 1_u64 << permission;
                    }
                }
            }
            plugins.insert(plugin_id.clone(), mask);
        }
        Some(Self { plugins })
    }

    fn to_json(&self) -> Value {
        let permissions = self
            .plugins
            .iter()
            .map(|(plugin_id, mask)| {
                let names = PERMISSION_NAMES
                    .iter()
                    .enumerate()
                    .filter(|(permission, _)| mask & (1_u64 << permission) != 0)
                    .map(|(_, name)| Value::String((*name).to_owned()))
                    .collect();
                (plugin_id.clone(), Value::Array(names))
            })
            .collect::<Map<_, _>>();
        Value::Object(permissions)
    }

    fn has(&self, plugin_id: &str, permission: i32) -> bool {
        permission_is_valid(permission)
            && self
                .plugins
                .get(plugin_id)
                .is_some_and(|mask| mask & (1_u64 << permission) != 0)
    }

    fn grant(&mut self, plugin_id: &str, permission: i32) -> bool {
        if !permission_is_valid(permission) {
            return false;
        }
        *self.plugins.entry(plugin_id.to_owned()).or_default() |= 1_u64 << permission;
        true
    }

    fn revoke(&mut self, plugin_id: &str, permission: i32) -> Option<bool> {
        if !permission_is_valid(permission) {
            return None;
        }
        let Some(mask) = self.plugins.get_mut(plugin_id) else {
            return Some(false);
        };
        *mask &= !(1_u64 << permission);
        if *mask == 0 {
            self.plugins.remove(plugin_id);
        }
        Some(true)
    }

    fn grant_all(&mut self, plugin_id: &str) {
        let all = (1_u64 << PERMISSION_NAMES.len()) - 1;
        self.plugins.insert(plugin_id.to_owned(), all);
    }

    fn revoke_all(&mut self, plugin_id: &str) {
        self.plugins.remove(plugin_id);
    }

    #[cfg(test)]
    fn mask(&self, plugin_id: &str) -> u64 {
        self.plugins.get(plugin_id).copied().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginPermissionState {
    state: PermissionState,
}

impl PluginPermissionState {
    pub fn from_value(value: &Value) -> Option<Self> {
        PermissionState::from_value(value).map(|state| Self { state })
    }

    pub fn snapshot(&self) -> Map<String, Value> {
        self.state
            .to_json()
            .as_object()
            .cloned()
            .expect("plugin permission snapshot must be an object")
    }

    pub fn has(&self, plugin_id: &str, permission: PluginPermission) -> bool {
        self.state.has(plugin_id, permission as i32)
    }

    pub fn set(&mut self, plugin_id: &str, permission: PluginPermission, granted: bool) {
        if granted {
            let _ = self.state.grant(plugin_id, permission as i32);
        } else {
            let _ = self.state.revoke(plugin_id, permission as i32);
        }
    }

    pub fn grant_all(&mut self, plugin_id: &str) {
        self.state.grant_all(plugin_id);
    }

    pub fn revoke_all(&mut self, plugin_id: &str) {
        self.state.revoke_all(plugin_id);
    }

    pub fn granted(&self, plugin_id: &str) -> Vec<PluginPermission> {
        PluginPermission::ALL
            .into_iter()
            .filter(|permission| self.has(plugin_id, *permission))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_normalizes_unknown_names_and_preserves_empty_plugin_entries() {
        let state = PermissionState::from_json(
            br#"{
                "plugin.b": ["clip.read", "unknown", "clip.read"],
                "plugin.a": "invalid"
            }"#,
        )
        .expect("valid permission document");
        assert!(state.has("plugin.b", 1));
        assert!(!state.has("plugin.b", 2));
        assert_eq!(state.mask("plugin.a"), 0);
        assert_eq!(
            state.to_json(),
            serde_json::json!({
                "plugin.a": [],
                "plugin.b": ["clip.read"]
            })
        );
    }

    #[test]
    fn mutations_are_idempotent_and_remove_empty_plugin_state() {
        let mut state = PermissionState::default();
        assert!(state.grant("plugin", 0));
        assert!(state.grant("plugin", 0));
        assert!(state.has("plugin", 0));
        assert_eq!(state.revoke("plugin", 1), Some(true));
        assert_eq!(state.mask("plugin"), 1);
        assert_eq!(state.revoke("plugin", 0), Some(true));
        assert!(!state.plugins.contains_key("plugin"));
        assert_eq!(state.revoke("plugin", 0), Some(false));
        assert_eq!(state.revoke("plugin", -1), None);

        state.grant_all("plugin");
        assert_eq!(
            state.mask("plugin").count_ones() as usize,
            PERMISSION_NAMES.len()
        );
        state.revoke_all("plugin");
        assert_eq!(state.mask("plugin"), 0);
    }
}

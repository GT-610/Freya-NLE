use crate::project_io::write_atomic;
use crate::settings::application_data_root;
use freya_nle_core::api::{
    EffectPreset, build_effect_preset, parse_effect_preset, preset_name_is_safe,
};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Upper bound for one preset definition.
///
/// A preset holds one effect's parameters, keyframes, and expressions, so it stays
/// far below this even with a long keyframe track. Presets live in a directory the
/// user shares with packages, so a preset is not necessarily a file this process
/// wrote; reading it must not allocate for whatever the file claims to be.
const MAX_PRESET_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct PresetStore {
    root: PathBuf,
}

impl PresetStore {
    pub fn load() -> Self {
        Self {
            root: default_preset_root(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn names(&self, effect_id: &str) -> Vec<String> {
        let Ok(directory) = self.effect_directory(effect_id, false) else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut names = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                (path.is_file()
                    && path.extension().and_then(|extension| extension.to_str()) == Some("json"))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
            })
            .filter(|name| preset_name_is_safe(name))
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    pub fn load_preset(&self, effect_id: &str, name: &str) -> Result<EffectPreset, String> {
        let path = self.preset_path(effect_id, name, false)?;
        let bytes = crate::file_io::read_bounded(&path, MAX_PRESET_BYTES).map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                format!(
                    "{}: preset exceeds the maximum allowed size of {MAX_PRESET_BYTES} bytes",
                    path.display()
                )
            } else {
                format!("{}: {error}", path.display())
            }
        })?;
        parse_effect_preset(&bytes, effect_id, name).map_err(|error| error.to_string())
    }

    pub fn save(
        &self,
        effect_id: &str,
        name: &str,
        params: Map<String, Value>,
        keyframes: Map<String, Value>,
        expressions: Map<String, Value>,
        enabled: bool,
    ) -> Result<(), String> {
        let path = self.preset_path(effect_id, name, true)?;
        let bytes = build_effect_preset(effect_id, name, enabled, params, keyframes, expressions)
            .map_err(|error| error.to_string())?;
        // Refuse before replacing anything: a preset this large would be rejected
        // on the next load, so writing it would only hide the problem.
        if bytes.len() as u64 > MAX_PRESET_BYTES {
            return Err(format!(
                "{}: preset exceeds the maximum allowed size of {MAX_PRESET_BYTES} bytes",
                path.display()
            ));
        }
        write_atomic(&path, &bytes).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn delete(&self, effect_id: &str, name: &str) -> Result<(), String> {
        let path = self.preset_path(effect_id, name, false)?;
        fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    fn preset_path(
        &self,
        effect_id: &str,
        name: &str,
        create_directory: bool,
    ) -> Result<PathBuf, String> {
        if !preset_name_is_safe(name) {
            return Err("preset name is not a safe path component".to_owned());
        }
        Ok(self
            .effect_directory(effect_id, create_directory)?
            .join(format!("{name}.json")))
    }

    fn effect_directory(&self, effect_id: &str, create: bool) -> Result<PathBuf, String> {
        if !preset_name_is_safe(effect_id) {
            return Err("effect id is not a safe path component".to_owned());
        }
        if create {
            fs::create_dir_all(&self.root)
                .map_err(|error| format!("{}: {error}", self.root.display()))?;
        }
        let directory = self.root.join(effect_id);
        if directory.exists() {
            let canonical_root = fs::canonicalize(&self.root)
                .map_err(|error| format!("{}: {error}", self.root.display()))?;
            let canonical_directory = fs::canonicalize(&directory)
                .map_err(|error| format!("{}: {error}", directory.display()))?;
            if !canonical_directory.starts_with(&canonical_root) {
                return Err("preset directory escapes the preset root".to_owned());
            }
        } else if create {
            fs::create_dir(&directory)
                .map_err(|error| format!("{}: {error}", directory.display()))?;
        }
        Ok(directory)
    }
}

fn default_preset_root() -> PathBuf {
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let portable = directory.join("presets");
        if portable.exists() || directory_is_writable(directory) {
            return portable;
        }
    }
    application_data_root().join("presets")
}

fn directory_is_writable(directory: &Path) -> bool {
    fs::metadata(directory)
        .map(|metadata| !metadata.permissions().readonly())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_list_delete_and_identity_checks_match_qt() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().to_path_buf();
        let store = PresetStore::from_root(root.clone());
        store
            .save(
                "blur",
                "Warm",
                serde_json::json!({"size": 5})
                    .as_object()
                    .cloned()
                    .expect("params"),
                Map::new(),
                Map::new(),
                true,
            )
            .expect("save preset");
        // The saved file records that it knows about expressions, so applying
        // it can clear a target expression the preset does not carry.
        let saved = store
            .load_preset("blur", "Warm")
            .expect("load saved preset");
        assert!(saved.records_expressions());
        assert_eq!(store.names("blur"), ["Warm"]);
        assert_eq!(
            store
                .load_preset("blur", "Warm")
                .expect("load preset")
                .params["size"],
            5
        );
        assert!(store.load_preset("mosaic", "Warm").is_err());
        store.delete("blur", "Warm").expect("delete preset");
        assert!(store.names("blur").is_empty());
        fs::remove_dir_all(root).expect("remove temporary preset root");
    }

    #[test]
    fn oversized_presets_are_rejected_before_the_allocation() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("presets");
        let store = PresetStore::from_root(root.clone());
        let directory = root.join("blur");
        fs::create_dir_all(&directory).expect("preset directory");

        // A preset is not necessarily written by this process: the preset root is
        // a directory the user shares with packaged effects, so the size is
        // checked before the file is read rather than after.
        let oversized = directory.join("Huge.json");
        let file = fs::File::create(&oversized).expect("oversized preset");
        file.set_len(MAX_PRESET_BYTES + 1).expect("preset grows");
        drop(file);
        let error = store
            .load_preset("blur", "Huge")
            .expect_err("an oversized preset is rejected");
        assert!(
            error.contains("maximum allowed size"),
            "the failure must name the limit: {error}"
        );

        // A file that is not a file at all would block a plain read.
        let fifo = directory.join("Pipe.json");
        #[cfg(unix)]
        {
            let status = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("mkfifo runs");
            assert!(status.success());
            assert!(store.load_preset("blur", "Pipe").is_err());
            fs::remove_file(&fifo).expect("fifo removes");
        }
        #[cfg(not(unix))]
        let _ = fifo;

        // The bound must not reject a preset that fits.
        store
            .save(
                "blur",
                "Small",
                serde_json::json!({"size": 3})
                    .as_object()
                    .cloned()
                    .expect("params"),
                Map::new(),
                Map::new(),
                true,
            )
            .expect("small preset saves");
        assert_eq!(
            store
                .load_preset("blur", "Small")
                .expect("small preset loads")
                .params["size"],
            3
        );

        fs::remove_dir_all(root).expect("remove temporary preset root");
    }
}

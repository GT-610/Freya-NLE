use freya_nle_core::api::{ProjectDocument, TimelineState};
use serde_json::{Map, Value};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// File extension a project is saved with.
pub const PROJECT_EXTENSION: &str = "freya";

/// Largest project or recovery document that is read or written, matching the bound the Qt
/// frontend applied before its retirement.
pub const MAX_PROJECT_DOCUMENT_BYTES: u64 = 256 * 1024 * 1024;

/// The extension projects were saved with before the rename. Still opened.
pub const LEGACY_PROJECT_EXTENSION: &str = "aviqtl";

pub struct ProjectSession {
    pub state: TimelineState,
    pub document: ProjectDocument,
    pub path: Option<PathBuf>,
    pub dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectDefaults {
    pub width: i32,
    pub height: i32,
    pub fps: f64,
    pub sample_rate: i32,
    pub duration: i32,
    pub enable_snap: bool,
    pub magnetic_snap_range: i32,
}

impl Default for ProjectDefaults {
    fn default() -> Self {
        Self {
            width: 1_920,
            height: 1_080,
            fps: 60.0,
            sample_rate: 48_000,
            duration: 3_600,
            enable_snap: true,
            magnetic_snap_range: 10,
        }
    }
}

impl ProjectSession {
    #[cfg(test)]
    pub fn blank() -> Self {
        Self::blank_with(ProjectDefaults::default())
    }

    pub fn blank_with(defaults: ProjectDefaults) -> Self {
        // Normalize caller input with the same bounds as the launcher settings
        // so the fixed document below always serializes and validates.
        let defaults = ProjectDefaults {
            width: defaults.width.clamp(1, 16_000),
            height: defaults.height.clamp(1, 16_000),
            fps: if defaults.fps.is_finite() {
                defaults.fps.clamp(1.0, 240.0)
            } else {
                60.0
            },
            sample_rate: defaults.sample_rate.clamp(8_000, 192_000),
            duration: defaults.duration.clamp(1, 1_000_000),
            enable_snap: defaults.enable_snap,
            magnetic_snap_range: defaults.magnetic_snap_range.clamp(1, 100),
        };
        let document = serde_json::json!({
            "version": 3,
            "settings": {
                "width": defaults.width,
                "height": defaults.height,
                "fps": defaults.fps,
                "sampleRate": defaults.sample_rate
            },
            "scenes": [{
                "id": 1,
                "name": "Root",
                "width": defaults.width,
                "height": defaults.height,
                "fps": defaults.fps,
                "duration": defaults.duration,
                "enableSnap": defaults.enable_snap,
                "magneticSnapRange": defaults.magnetic_snap_range
            }],
            "clips": []
        });
        let bytes = serde_json::to_vec(&document)
            .expect("the built-in blank project document must serialize");
        let state =
            TimelineState::from_json(&bytes).expect("the built-in blank project must remain valid");
        let document = state.snapshot();
        Self {
            state,
            document,
            path: None,
            dirty: false,
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = read_document(path, MAX_PROJECT_DOCUMENT_BYTES)?;
        let mut project =
            Self::from_json(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        project.path = Some(path.to_path_buf());
        Ok(project)
    }

    pub fn from_json(input: &[u8]) -> Result<Self, String> {
        // A fragment is a block of clips, not a project: it has no scenes, so reading one as
        // a project would open an empty timeline and hide the work it holds.
        if crate::fragment::is_fragment_document(input) {
            return Err(
                "This is a fragment, not a project. Use File \u{2192} Insert Fragment to add \
                 its clips to a project."
                    .to_owned(),
            );
        }
        let state = TimelineState::from_json(input).map_err(|error| error.to_string())?;
        let document = state.snapshot();
        Ok(Self {
            state,
            document,
            path: None,
            dirty: false,
        })
    }

    pub fn load_recovery(
        snapshot_path: &Path,
        _original_project_url: &str,
    ) -> Result<Self, String> {
        let bytes = read_document(snapshot_path, MAX_PROJECT_DOCUMENT_BYTES)?;
        let state = TimelineState::from_json(&bytes)
            .map_err(|error| format!("{}: {error}", snapshot_path.display()))?;
        let document = state.snapshot();
        Ok(Self {
            state,
            document,
            // Match the Qt recovery flow: the source path is informative only. A recovered
            // project is intentionally unsaved so that a normal Save opens Save As instead of
            // overwriting the original project.
            path: None,
            dirty: true,
        })
    }

    pub fn refresh(&mut self) {
        self.document = self.state.snapshot();
    }

    pub fn save(&mut self) -> Result<(), String> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| "choose a project path before saving".to_owned())?;
        let bytes = self.snapshot_bytes_for(&path)?;
        write_atomic(&path, &bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        self.dirty = false;
        Ok(())
    }

    pub fn save_as(&mut self, path: &Path) -> Result<(), String> {
        let bytes = self.snapshot_bytes_for(path)?;
        write_atomic(path, &bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }

    pub fn snapshot_bytes(&self) -> Result<Vec<u8>, String> {
        let json = self
            .state
            .snapshot_json()
            .map_err(|error| error.to_string())?;
        serde_json::to_vec_pretty(&json).map_err(|error| error.to_string())
    }

    /// Serializes the project for `path`, storing media that lives beside it with a relative
    /// path. A project and its media then move together instead of breaking at the old location.
    fn snapshot_bytes_for(&self, path: &Path) -> Result<Vec<u8>, String> {
        let mut json = self
            .state
            .snapshot_json()
            .map_err(|error| error.to_string())?;
        if let Some(directory) = path.parent() {
            relativize_media_paths(&mut json, directory);
        }
        let bytes = serde_json::to_vec_pretty(&json).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_PROJECT_DOCUMENT_BYTES {
            return Err(format!(
                "{}: project exceeds the maximum allowed size of {MAX_PROJECT_DOCUMENT_BYTES} bytes",
                path.display()
            ));
        }
        Ok(bytes)
    }
}

fn read_document(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.len() > limit {
        return Err(document_too_large(path, limit));
    }
    let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(document_too_large(path, limit));
    }
    Ok(bytes)
}

fn document_too_large(path: &Path, limit: u64) -> String {
    format!(
        "{}: file exceeds the maximum allowed size of {limit} bytes",
        path.display()
    )
}

/// Rewrites the media parameters of every clip that point inside `project_directory` to
/// project-relative paths. Files outside the directory keep their absolute path, which keeps a
/// project that references shared footage stable when it is saved elsewhere.
fn relativize_media_paths(document: &mut Value, project_directory: &Path) {
    let Ok(directory) = std::path::absolute(project_directory) else {
        return;
    };
    let Some(clips) = document.get_mut("clips").and_then(Value::as_array_mut) else {
        return;
    };
    for clip in clips {
        if let Some(params) = clip.get_mut("params").and_then(Value::as_object_mut) {
            for (object, key) in [("video", "path"), ("image", "path"), ("audio", "source")] {
                let Some(media) = params.get_mut(object).and_then(Value::as_object_mut) else {
                    continue;
                };
                relativize_parameter(media, key, &directory);
            }
        }
        let Some(effects) = clip.get_mut("effects").and_then(Value::as_array_mut) else {
            continue;
        };
        for effect in effects {
            let key = match effect.get("id").and_then(Value::as_str) {
                Some("video" | "image") => "path",
                Some("audio") => "source",
                _ => continue,
            };
            if let Some(params) = effect.get_mut("params").and_then(Value::as_object_mut) {
                relativize_parameter(params, key, &directory);
            }
        }
    }
}

fn relativize_parameter(params: &mut Map<String, Value>, key: &str, directory: &Path) {
    let Some(Value::String(path)) = params.get(key) else {
        return;
    };
    if path.is_empty() {
        return;
    }
    let absolute = Path::new(path);
    if !absolute.is_absolute() {
        return;
    }
    let Ok(absolute) = std::path::absolute(absolute) else {
        return;
    };
    let Ok(relative) = absolute.strip_prefix(directory) else {
        return;
    };
    let relative = relative.to_string_lossy().replace('\\', "/");
    if !relative.is_empty() {
        params.insert(key.to_owned(), Value::String(relative));
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".freya-nle-save-")
        .tempfile_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    // Windows can reject simultaneous replace operations on the same target.
    // Serialize only publication; encoding and disk writes remain concurrent.
    static PUBLICATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _publication = PUBLICATION
        .lock()
        .map_err(|_| io::Error::other("save publication lock poisoned"))?;
    // Close the source handle before replacement, including on Windows where
    // a still-open replaced file can remain delete-pending during another save.
    temporary
        .into_temp_path()
        .persist(path)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use freya_nle_core::api::TimelineCommand;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn a_fragment_is_refused_as_a_project_and_says_what_to_do() {
        // A fragment has clips but no scenes, so opening one as a project would produce an
        // empty timeline and hide everything the file holds.
        let fragment = crate::fragment::FragmentDocument::new(
            &ProjectSession::blank().document,
            &ProjectSession::blank().document.scenes[0],
            Vec::new(),
        );
        let bytes = fragment.to_bytes().expect("fragment serializes");
        let message = match ProjectSession::from_json(&bytes) {
            Ok(_) => panic!("a fragment is not a project"),
            Err(message) => message,
        };
        assert!(message.contains("fragment"), "{message}");
        assert!(message.contains("Insert Fragment"), "{message}");

        // A real project still opens.
        let project = ProjectSession::blank();
        let bytes = project.snapshot_bytes().expect("project serializes");
        assert!(ProjectSession::from_json(&bytes).is_ok());
    }

    #[test]
    fn concurrent_saves_publish_complete_files_and_remove_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("project.aviqtl");
        write_atomic(&target, b"old project").unwrap();
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for byte in 1..=8_u8 {
                let target = &target;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    write_atomic(target, &vec![byte; 64 * 1024]).unwrap();
                });
            }
        });
        let bytes = fs::read(&target).unwrap();
        assert_eq!(bytes.len(), 64 * 1024);
        assert!((1..=8).contains(&bytes[0]));
        assert!(bytes.iter().all(|byte| *byte == bytes[0]));
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_save_as_keeps_the_old_project_and_dirty_state() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("project.aviqtl");
        let blocked = directory.path().join("directory.aviqtl");
        fs::create_dir(&blocked).unwrap();
        let mut project = ProjectSession::blank();
        project.save_as(&target).unwrap();
        let original = fs::read(&target).unwrap();
        project.dirty = true;
        assert!(project.save_as(&blocked).is_err());
        assert!(
            project
                .save_as(&directory.path().join("missing/project.aviqtl"))
                .is_err()
        );
        assert!(project.dirty);
        assert_eq!(project.path.as_deref(), Some(target.as_path()));
        assert_eq!(fs::read(&target).unwrap(), original);
        assert!(blocked.is_dir());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn blank_project_uses_configured_defaults() {
        let project = ProjectSession::blank_with(ProjectDefaults {
            width: 1_280,
            height: 720,
            fps: 30.0,
            sample_rate: 44_100,
            duration: 900,
            enable_snap: false,
            magnetic_snap_range: 18,
        });
        assert_eq!(project.document.settings.width, 1_280);
        assert_eq!(project.document.settings.height, 720);
        assert_eq!(project.document.settings.fps, 30.0);
        assert_eq!(project.document.settings.sample_rate, 44_100);
        assert_eq!(project.document.scenes[0].duration, 900);
        assert_eq!(project.document.scenes[0].fps, 30.0);
        assert!(!project.document.scenes[0].enable_snap);
        assert_eq!(project.document.scenes[0].magnetic_snap_range, 18);
    }

    #[test]
    fn save_and_reopen_preserves_a_typed_edit() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "freya-nle-egui-project-{}-{nonce}.aviqtl",
            std::process::id()
        ));
        let mut project = ProjectSession::blank();
        let mut scene = project.document.scenes[0].clone();
        scene.name = "Edited in Rust".to_owned();
        let transaction = project
            .state
            .plan(TimelineCommand::UpdateScene {
                scene_id: scene.id,
                scene,
            })
            .expect("scene edit plans");
        project
            .state
            .apply(&transaction)
            .expect("scene edit applies");
        project.refresh();
        project.path = Some(path.clone());
        project.dirty = true;
        project.save().expect("project saves");

        let reopened = ProjectSession::load(&path).expect("saved project reopens");
        assert_eq!(reopened.document.scenes[0].name, "Edited in Rust");
        assert!(!reopened.dirty);
        fs::remove_file(path).expect("test project removes");
    }

    #[test]
    fn failed_save_as_preserves_the_previous_path_and_dirty_state() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "freya-nle-save-as-directory-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("fixture directory creates");
        let mut project = ProjectSession::blank();
        project.dirty = true;

        assert!(project.save_as(&path).is_err());
        assert_eq!(project.path, None);
        assert!(project.dirty);

        fs::remove_dir(path).expect("fixture directory removes");
    }

    #[test]
    fn recovery_stays_unsaved_and_dirty_even_when_the_original_path_is_known() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows the Unix epoch")
            .as_nanos();
        let snapshot_path = std::env::temp_dir().join(format!(
            "freya-nle-egui-recovery-{}-{nonce}.aviqtl",
            std::process::id()
        ));
        let project = ProjectSession::blank();
        fs::write(
            &snapshot_path,
            project.snapshot_bytes().expect("snapshot serializes"),
        )
        .expect("recovery snapshot writes");

        let recovered =
            ProjectSession::load_recovery(&snapshot_path, "file:///tmp/AviQtl%20recovery.aviqtl")
                .expect("recovery snapshot loads");
        assert_eq!(recovered.path, None);
        assert!(recovered.dirty);

        let unsaved = ProjectSession::load_recovery(&snapshot_path, "")
            .expect("unsaved recovery snapshot loads");
        assert_eq!(unsaved.path, None);
        assert!(unsaved.dirty);
        fs::remove_file(snapshot_path).expect("recovery snapshot removes");
    }

    #[test]
    fn saving_stores_media_inside_the_project_directory_as_a_relative_path() {
        let directory = tempfile::tempdir().unwrap();
        let media_directory = directory.path().join("media");
        fs::create_dir(&media_directory).unwrap();
        let video = media_directory.join("clip.mp4");
        let audio = media_directory.join("music.wav");
        fs::write(&video, b"video").unwrap();
        fs::write(&audio, b"audio").unwrap();
        let outside = std::env::temp_dir().join("freya-nle-shared-footage.mp4");

        let mut project = ProjectSession::blank();
        let mut document = project.state.snapshot_json().expect("snapshot serializes");
        let clips = document["clips"]
            .as_array_mut()
            .expect("a project snapshot has a clip list");
        clips.push(serde_json::json!({
            "id": 1,
            "sceneId": 1,
            "type": "video",
            "start": 0,
            "duration": 100,
            "layer": 0,
            "effects": [
                {"enabled": true, "id": "video", "params": {"path": video.to_string_lossy()}},
                {"enabled": true, "id": "audio", "params": {"source": audio.to_string_lossy()}}
            ]
        }));
        clips.push(serde_json::json!({
            "id": 2,
            "sceneId": 1,
            "type": "video",
            "start": 120,
            "duration": 100,
            "layer": 1,
            "effects": [
                {"enabled": true, "id": "video", "params": {"path": outside.to_string_lossy()}}
            ]
        }));
        project =
            ProjectSession::from_json(&serde_json::to_vec(&document).expect("document serializes"))
                .expect("the media project opens");
        let target = directory.path().join("project.freya");
        project.path = Some(target.clone());
        project.save().expect("project saves");

        let saved: Value = serde_json::from_slice(&fs::read(&target).expect("saved project reads"))
            .expect("saved project parses");
        assert_eq!(
            saved["clips"][0]["effects"][0]["params"]["path"],
            Value::String("media/clip.mp4".to_owned())
        );
        assert_eq!(
            saved["clips"][0]["effects"][1]["params"]["source"],
            Value::String("media/music.wav".to_owned())
        );
        assert_eq!(
            saved["clips"][1]["effects"][0]["params"]["path"],
            Value::String(outside.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn documents_over_the_size_limit_are_refused_before_they_are_read() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project.freya");
        fs::write(&path, b"12345").unwrap();
        let error = read_document(&path, 4).expect_err("an oversized document is refused");
        assert!(error.contains("maximum allowed size"), "{error}");
        assert_eq!(
            read_document(&path, 5).expect("a document at the limit reads"),
            b"12345"
        );
    }
}

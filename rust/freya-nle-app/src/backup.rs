//! Timestamped copies of saved projects, like AviUtl2's バックアップ履歴.
//!
//! The recovery store protects unsaved work from a crash; this store keeps the
//! versions a user actually saved, so an accidental overwrite can still be
//! undone. Copies live under one directory per project, keyed by a hash of the
//! canonical project path, and the newest `limit` entries are kept.

use crate::project_io::{MAX_PROJECT_DOCUMENT_BYTES, write_atomic};
use crate::recovery::iso_timestamp;
#[cfg(not(test))]
use crate::settings::application_data_root;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many saved versions one project keeps by default.
pub const DEFAULT_BACKUP_HISTORY_LIMIT: usize = 20;
/// How much disk the whole backup store may take by default.
pub const DEFAULT_BACKUP_TOTAL_LIMIT_MB: u64 = 512;
/// The smallest budget the settings allow, so pruning cannot wipe the store.
pub const MIN_BACKUP_TOTAL_LIMIT_MB: u64 = 16;
/// The largest budget the settings allow.
pub const MAX_BACKUP_TOTAL_LIMIT_MB: u64 = 16_384;

const BACKUP_EXTENSION: &str = "freya";
/// Records which project a backup directory belongs to, so the history can be
/// browsed across projects. Older directories simply have no copy yet.
const MANIFEST_NAME: &str = "project.json";

/// One saved version on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    pub file_name: String,
    pub saved_at: String,
    pub size: u64,
}

/// One project's backup directory, as the all-projects browser sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupProject {
    /// Opaque directory name; the UI hands it back to open or delete.
    pub key: String,
    /// Project file stem from the manifest; empty when the directory predates
    /// the manifest and the UI should show its own fallback label.
    pub label: String,
    /// Canonical project path, when the manifest recorded one.
    pub path: Option<String>,
    /// Newest first.
    pub entries: Vec<BackupEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProjectManifest {
    path: String,
    name: String,
}

/// The directory versions are written to when the user has not chosen one.
#[cfg(not(test))]
pub fn default_backup_root() -> PathBuf {
    application_data_root().join("backups")
}

/// Resolves the configured directory against one raw setting value.
///
/// An empty value means "the default location"; a relative path is rejected
/// because its meaning would depend on the working directory of the process.
pub fn backup_root_from_setting(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        #[cfg(not(test))]
        {
            return Some(default_backup_root());
        }
        #[cfg(test)]
        {
            return None;
        }
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return None;
    }
    Some(path.to_path_buf())
}

/// Bytes the on-disk backup store takes, for the settings page.
///
/// A UI thread reads this once per page open, so it goes through the same
/// scanner the pruning uses instead of keeping a running total in settings.
pub fn stored_bytes() -> u64 {
    #[cfg(not(test))]
    {
        BackupStore::new().total_size()
    }
    #[cfg(test)]
    {
        0
    }
}

/// Directory-backed store for saved project versions.
#[derive(Debug)]
pub(crate) struct BackupStore {
    root: PathBuf,
}

impl BackupStore {
    #[cfg(not(test))]
    pub(crate) fn new() -> Self {
        Self::with_root(default_backup_root())
    }

    pub(crate) fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    /// Points the store at a user-chosen directory.
    ///
    /// The directory is created on demand, so a typo only fails when a version
    /// is actually written instead of preventing the editor from starting.
    pub(crate) fn set_root(&mut self, root: PathBuf) {
        self.root = root;
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The directory name that holds one project's backups.
    pub(crate) fn key_for(&self, project_path: &Path) -> String {
        self.project_dir(project_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The directory that holds one project's backups.
    ///
    /// The key is a hash of the project path, so the spelling has to be
    /// normalized first: `/tmp/a/../b.freya` and `/tmp/b.freya` are the same
    /// project and must share one history.
    fn project_dir(&self, project_path: &Path) -> PathBuf {
        let normalized = normalize_project_path(project_path);
        let mut hasher = Sha256::new();
        hasher.update(normalized.to_string_lossy().as_bytes());
        let digest = hasher.finalize();
        let key = digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.root.join(key)
    }

    /// Resolves a project path to its backup directory, refusing a symlink.
    ///
    /// [`Self::project_dir`] only hashes the path, so every caller that reads
    /// or writes through the result has to check it: a key directory replaced
    /// by a link would otherwise let listing, pruning and deleting reach files
    /// outside the backup root.
    fn checked_project_dir(&self, project_path: &Path) -> Result<PathBuf, String> {
        let directory = self.project_dir(project_path);
        let key = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.key_directory(&key)
    }

    /// Resolves a directory key to a real directory under the backup root.
    ///
    /// A key is 16 hex characters, so it cannot name a path outside the root by
    /// spelling alone — but it can name a *symlink* that points outside. Listing
    /// or deleting through such a link would touch files the user never put in
    /// the backup folder, so a linked directory is refused instead of followed.
    fn key_directory(&self, key: &str) -> Result<PathBuf, String> {
        if !valid_project_key(key) {
            return Err(format!("{key:?} is not a backup directory key"));
        }
        let directory = self.root.join(key);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
                "{} is a symbolic link, which a backup directory must not be",
                directory.display()
            )),
            _ => Ok(directory),
        }
    }

    /// Removes the oldest versions across every project until the whole store
    /// fits `budget_bytes`.
    ///
    /// The newest version overall always survives, so a budget smaller than one
    /// project still leaves a single restorable copy instead of an empty list.
    /// Returns how many files were deleted.
    pub(crate) fn enforce_total_budget(&self, budget_bytes: u64) -> Result<usize, String> {
        let Ok(read_dir) = fs::read_dir(&self.root) else {
            return Ok(0);
        };
        let mut versions = read_dir
            .flatten()
            .filter(|entry| valid_project_key(&entry.file_name().to_string_lossy()))
            // A linked directory may point anywhere, so its contents are not
            // this store's to count or delete.
            .filter(|entry| !entry.file_type().is_ok_and(|kind| kind.is_symlink()))
            .flat_map(|entry| {
                let directory = entry.path();
                list_entries(&directory)
                    .into_iter()
                    .map(move |backup| (directory.clone(), backup))
            })
            .collect::<Vec<_>>();
        if versions.is_empty() {
            return Ok(0);
        }
        let mut total = versions.iter().map(|(_, entry)| entry.size).sum::<u64>();
        versions.sort_by(|left, right| left.1.file_name.cmp(&right.1.file_name));
        let mut removed = 0;
        // The last element is the newest version anywhere, and it stays even
        // when it alone exceeds the budget.
        for (directory, entry) in versions.iter().take(versions.len() - 1) {
            if total <= budget_bytes {
                break;
            }
            let path = directory.join(&entry.file_name);
            match fs::remove_file(&path) {
                Ok(()) => {
                    total = total.saturating_sub(entry.size);
                    removed += 1;
                }
                Err(error) => return Err(format!("{}: {error}", path.display())),
            }
        }
        Ok(removed)
    }

    /// The bytes every stored version takes, for the settings description.
    pub(crate) fn total_size(&self) -> u64 {
        let Ok(read_dir) = fs::read_dir(&self.root) else {
            return 0;
        };
        read_dir
            .flatten()
            .filter(|entry| valid_project_key(&entry.file_name().to_string_lossy()))
            .filter(|entry| !entry.file_type().is_ok_and(|kind| kind.is_symlink()))
            .flat_map(|entry| list_entries(&entry.path()))
            .map(|entry| entry.size)
            .sum()
    }

    /// Writes one copy and prunes older ones. A `limit` of zero disables it.
    pub(crate) fn record(
        &self,
        project_path: &Path,
        bytes: &[u8],
        limit: usize,
    ) -> Result<Option<BackupEntry>, String> {
        if limit == 0 {
            return Ok(None);
        }
        // A document past the read bound could never be restored, so storing it
        // would produce a version that looks saved and cannot be opened.
        if bytes.len() as u64 > MAX_PROJECT_DOCUMENT_BYTES {
            return Err(format!(
                "the project is larger than the {} byte backup limit and would not be restorable",
                MAX_PROJECT_DOCUMENT_BYTES
            ));
        }
        let directory = self.checked_project_dir(project_path)?;
        fs::create_dir_all(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        // Best effort: the backup itself must still be written if the manifest
        // cannot be updated, because the restored data does not depend on it.
        let _ = write_manifest(&directory, project_path);
        let stamp = backup_millis(SystemTime::now());
        let mut file_name = format!("{stamp:013}.{BACKUP_EXTENSION}");
        let mut path = directory.join(&file_name);
        let mut suffix = 1;
        while path.exists() {
            file_name = format!("{stamp:013}-{suffix}.{BACKUP_EXTENSION}");
            path = directory.join(&file_name);
            suffix += 1;
            if suffix > 100 {
                return Err(format!(
                    "{}: too many backups in the same millisecond",
                    directory.display()
                ));
            }
        }
        write_atomic(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        self.prune(&directory, limit)?;
        Ok(Some(BackupEntry {
            file_name,
            saved_at: iso_timestamp(
                SystemTime::UNIX_EPOCH
                    + Duration::from_millis(u64::try_from(stamp).unwrap_or(u64::MAX)),
            ),
            size: bytes.len() as u64,
        }))
    }

    /// Newest first.
    pub(crate) fn entries(&self, project_path: &Path) -> Vec<BackupEntry> {
        match self.checked_project_dir(project_path) {
            Ok(directory) => list_entries(&directory),
            Err(_) => Vec::new(),
        }
    }

    /// Every project that has at least one backup, newest project first.
    ///
    /// Directories written before the manifest existed still show up, with an
    /// empty label and no path; the window falls back to its own wording.
    pub(crate) fn projects(&self) -> Vec<BackupProject> {
        let Ok(read_dir) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut projects = read_dir
            .flatten()
            .filter_map(|entry| {
                let key = entry.file_name().to_string_lossy().into_owned();
                if !valid_project_key(&key) || entry.file_type().is_ok_and(|kind| kind.is_symlink())
                {
                    return None;
                }
                let entries = list_entries(&entry.path());
                if entries.is_empty() {
                    return None;
                }
                let manifest = read_manifest(&entry.path());
                Some(BackupProject {
                    key,
                    label: manifest
                        .as_ref()
                        .map_or_else(String::new, |m| m.name.clone()),
                    path: manifest.map(|m| m.path),
                    entries,
                })
            })
            .collect::<Vec<_>>();
        projects.sort_by(|left, right| right.entries[0].file_name.cmp(&left.entries[0].file_name));
        projects
    }

    /// Removes every version of one project and its directory.
    pub(crate) fn clear_key(&self, key: &str) -> Result<usize, String> {
        let directory = self.key_directory(key)?;
        let removed = list_entries(&directory).len();
        match fs::remove_dir_all(&directory) {
            Ok(()) => Ok(removed),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(format!("{}: {error}", directory.display())),
        }
    }

    /// The project path recorded in a directory's manifest, or empty.
    ///
    /// A restored session is unsaved, so this is the only place its original
    /// directory survives; relative media resolves against it.
    pub(crate) fn project_manifest_path(&self, key: &str) -> Option<String> {
        let directory = self.key_directory(key).ok()?;
        read_manifest(&directory)
            .map(|manifest| manifest.path)
            .filter(|path| !path.is_empty())
    }

    /// The project name recorded for a directory, when it has a manifest.
    pub(crate) fn project_label(&self, key: &str) -> Option<String> {
        let directory = self.key_directory(key).ok()?;
        read_manifest(&directory).map(|manifest| manifest.name)
    }

    /// Removes one version addressed by its directory key.
    pub(crate) fn remove_key(&self, key: &str, file_name: &str) -> Result<(), String> {
        let path = self.path_for_key(key, file_name)?;
        fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Resolves a directory key handed back by the UI. Both parts are checked
    /// here, so a forged key cannot reach outside the backup root.
    pub(crate) fn path_for_key(&self, key: &str, file_name: &str) -> Result<PathBuf, String> {
        let directory = self.key_directory(key)?;
        if !valid_backup_file(file_name) {
            return Err(format!("{file_name:?} is not a backup file name"));
        }
        Ok(directory.join(file_name))
    }

    pub(crate) fn path_for(&self, project_path: &Path, file_name: &str) -> Result<PathBuf, String> {
        if !valid_backup_file(file_name) {
            return Err(format!("{file_name:?} is not a backup file name"));
        }
        let directory = self.checked_project_dir(project_path)?;
        Ok(directory.join(file_name))
    }

    #[cfg(test)]
    pub(crate) fn read(&self, project_path: &Path, file_name: &str) -> Result<Vec<u8>, String> {
        let path = self.path_for(project_path, file_name)?;
        let metadata =
            fs::metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        if metadata.len() > MAX_PROJECT_DOCUMENT_BYTES {
            return Err(format!(
                "{}: backup exceeds the maximum allowed size",
                path.display()
            ));
        }
        fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub(crate) fn remove(&self, project_path: &Path, file_name: &str) -> Result<(), String> {
        let path = self.path_for(project_path, file_name)?;
        fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    fn prune(&self, directory: &Path, limit: usize) -> Result<(), String> {
        let entries = list_entries(directory)
            .into_iter()
            .skip(limit)
            .collect::<Vec<_>>();
        for entry in entries {
            let path = directory.join(&entry.file_name);
            if let Err(error) = fs::remove_file(&path) {
                return Err(format!("{}: {error}", path.display()));
            }
        }
        Ok(())
    }
}

fn list_entries(directory: &Path) -> Vec<BackupEntry> {
    let Ok(read_dir) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut entries = read_dir
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !valid_backup_file(&file_name) {
                return None;
            }
            let stamp = stamp_from_file(&file_name)?;
            Some(BackupEntry {
                file_name,
                saved_at: iso_timestamp(
                    SystemTime::UNIX_EPOCH
                        + Duration::from_millis(u64::try_from(stamp).unwrap_or(u64::MAX)),
                ),
                size: entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| right.file_name.cmp(&left.file_name));
    entries
}

fn write_manifest(directory: &Path, project_path: &Path) -> Result<(), String> {
    let manifest = ProjectManifest {
        path: project_path.to_string_lossy().into_owned(),
        name: project_path
            .file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned()),
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|error| error.to_string())?;
    write_atomic(&directory.join(MANIFEST_NAME), &bytes)
        .map_err(|error| format!("{}: {error}", directory.display()))
}

fn read_manifest(directory: &Path) -> Option<ProjectManifest> {
    let bytes = fs::read(directory.join(MANIFEST_NAME)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Lexically normalizes a project path so two spellings hash alike.
///
/// `canonicalize` would resolve symlinks and fail on a path that does not exist
/// yet, neither of which a backup key wants; the components are folded instead.
fn normalize_project_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                _ => normalized.push(component),
            },
            other => normalized.push(other),
        }
    }
    normalized
}

fn valid_project_key(key: &str) -> bool {
    key.len() == 16 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn backup_millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn stamp_from_file(file_name: &str) -> Option<u128> {
    file_name
        .strip_suffix(".freya")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

fn valid_backup_file(file_name: &str) -> bool {
    let Some(stem) = file_name.strip_suffix(".freya") else {
        return false;
    };
    let (stamp, suffix) = match stem.split_once('-') {
        Some((stamp, suffix)) => (stamp, Some(suffix)),
        None => (stem, None),
    };
    stamp.len() == 13
        && stamp.bytes().all(|byte| byte.is_ascii_digit())
        && suffix.is_none_or(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "freya-nle-app-backup-{name}-{}-{}",
            std::process::id(),
            backup_millis(SystemTime::now())
        ))
    }

    #[test]
    fn records_newest_first_and_prunes_beyond_the_limit() {
        let root = test_root("prune");
        let store = BackupStore::with_root(root.clone());
        let project = root.join("project.freya");
        for index in 0..5 {
            store
                .record(&project, format!("version {index}").as_bytes(), 3)
                .expect("backup records");
        }
        let entries = store.entries(&project);
        assert_eq!(entries.len(), 3);
        let newest = store
            .read(&project, &entries[0].file_name)
            .expect("newest backup reads");
        assert_eq!(newest, b"version 4");
        let oldest = store
            .read(&project, &entries[2].file_name)
            .expect("oldest kept backup reads");
        assert_eq!(oldest, b"version 2");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_path_traversal_and_foreign_file_names() {
        let root = test_root("names");
        let store = BackupStore::with_root(root.clone());
        for name in ["../escape.freya", "1234567890123.txt", "not-a-backup.freya"] {
            assert!(
                store
                    .path_for(Path::new("/tmp/project.freya"), name)
                    .is_err()
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Two spellings of one project must share a version history.
    #[test]
    fn a_project_key_ignores_the_spelling_of_its_path() {
        let root = test_root("normalize");
        let store = BackupStore::with_root(root.clone());
        let plain = store.key_for(Path::new("/tmp/work/demo.freya"));
        for spelling in [
            "/tmp/work/./demo.freya",
            "/tmp/work/elsewhere/../demo.freya",
            "/tmp//work/demo.freya",
        ] {
            assert_eq!(
                store.key_for(Path::new(spelling)),
                plain,
                "{spelling} should hash to the same key"
            );
        }
        // A genuinely different path keeps its own key.
        assert_ne!(
            store.key_for(Path::new("/tmp/work/other.freya")),
            plain,
            "different projects stay separate"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A key is 16 hex characters, so it cannot spell a path outside the root
    /// — but it can name a symlink that points outside. Scanning or pruning
    /// through such a link would enumerate and delete files the user never put
    /// in the backup folder.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_backup_directory_is_refused_rather_than_followed() {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink");
        let store = BackupStore::with_root(root.clone());
        let outside = root.with_extension("outside");
        fs::create_dir_all(&outside).expect("outside directory creates");
        let stray = outside.join("stray.freya");
        fs::write(&stray, b"not a backup").expect("outside file writes");

        let key = store.key_for(Path::new("/tmp/project.freya"));
        fs::create_dir_all(&root).expect("root creates");
        symlink(&outside, root.join(&key)).expect("directory symlink creates");

        assert!(store.key_directory(&key).is_err(), "the key is refused");
        assert!(store.clear_key(&key).is_err(), "clearing is refused");
        assert!(store.project_label(&key).is_none(), "no label is read");
        // A name that `valid_backup_file` accepts, so the refusal below can
        // only come from the symlink check.
        assert!(store.path_for_key(&key, "1234567890123.freya").is_err());
        // The path-based entry points build the directory by hashing, so they
        // have to refuse the link too rather than write through it.
        let project = Path::new("/tmp/project.freya");
        assert!(store.record(project, b"body", 5).is_err(), "record refuses");
        assert!(store.entries(project).is_empty(), "entries does not list");
        assert!(
            store.path_for(project, "1234567890123.freya").is_err(),
            "path_for refuses"
        );
        // A budget sweep must not walk into the target either.
        store.enforce_total_budget(0).expect("sweep completes");
        assert!(store.projects().is_empty(), "the link is not a project");
        assert!(stray.exists(), "the file outside the store survived");

        let _ = fs::remove_file(stray);
        let _ = fs::remove_dir_all(&outside);
        let _ = fs::remove_file(root.join(&key));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn projects_lists_every_backup_directory_with_its_project_name() {
        let root = test_root("projects");
        let store = BackupStore::with_root(root.clone());
        store
            .record(&root.join("one.freya"), b"version one", 5)
            .expect("first project records");
        store
            .record(&root.join("two.freya"), b"version two", 5)
            .expect("second project records");
        let projects = store.projects();
        assert_eq!(projects.len(), 2);
        let mut labels = projects
            .iter()
            .map(|project| project.label.as_str())
            .collect::<Vec<_>>();
        labels.sort_unstable();
        assert_eq!(labels, ["one", "two"]);
        assert!(projects.iter().all(|project| project.path.is_some()));
        for project in &projects {
            assert_eq!(project.entries.len(), 1);
            let path = store
                .path_for_key(&project.key, &project.entries[0].file_name)
                .expect("the key resolves");
            assert!(path.exists(), "{} should exist", path.display());
        }
        assert!(
            store
                .path_for_key("../escape", "1234567890123.freya")
                .is_err(),
            "a forged directory key stays inside the root"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_directory_without_a_manifest_still_lists_its_versions() {
        let root = test_root("legacy");
        let directory = root.join("0123456789abcdef");
        fs::create_dir_all(&directory).expect("legacy directory");
        fs::write(directory.join("1234567890123.freya"), b"legacy").expect("legacy backup");
        let store = BackupStore::with_root(root.clone());
        let projects = store.projects();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].key, "0123456789abcdef");
        assert_eq!(projects[0].label, "");
        assert_eq!(projects[0].path, None);
        assert_eq!(projects[0].entries.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_configured_root_replaces_where_versions_are_written() {
        let root = test_root("custom-root");
        let mut store = BackupStore::with_root(root.join("default"));
        let project = root.join("project.freya");
        store.record(&project, b"first", 5).expect("records");
        assert_eq!(store.entries(&project).len(), 1);

        let custom = root.join("external");
        store.set_root(custom.clone());
        assert_eq!(store.root(), custom.as_path());
        // The new location starts empty; nothing was moved behind the user.
        assert!(store.projects().is_empty());
        store.record(&project, b"second", 5).expect("records again");
        assert_eq!(store.projects().len(), 1);
        assert!(custom.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_setting_only_accepts_an_absolute_directory() {
        // `/tmp/backups` is absolute on Unix and merely a relative path on
        // Windows, so the fixture has to be built by the host: an absolute
        // path is what the setting promises to accept, and the check is one.
        let absolute = std::env::temp_dir().join("backups");
        let absolute = absolute.to_str().expect("the temp directory is UTF-8");
        assert!(backup_root_from_setting(absolute).is_some());
        assert_eq!(
            backup_root_from_setting(absolute),
            Some(PathBuf::from(absolute))
        );
        let padded = format!("  {absolute}  ");
        assert_eq!(
            backup_root_from_setting(&padded),
            Some(PathBuf::from(absolute)),
            "surrounding whitespace is trimmed before the check"
        );

        assert!(backup_root_from_setting("relative/backups").is_none());
        assert!(backup_root_from_setting(" ").is_none());
    }

    #[test]
    fn clearing_a_project_removes_its_directory_and_every_version() {
        let root = test_root("clear");
        let store = BackupStore::with_root(root.clone());
        store
            .record(&root.join("one.freya"), b"first", 5)
            .expect("records");
        store
            .record(&root.join("two.freya"), b"second", 5)
            .expect("records");
        let key = store.key_for(&root.join("one.freya"));
        assert_eq!(store.clear_key(&key).expect("clears"), 1);
        // The directory is gone, so the project stops being listed.
        assert_eq!(store.projects().len(), 1);
        assert_eq!(store.clear_key(&key).expect("clearing again is a no-op"), 0);
        assert!(store.clear_key("../escape").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_total_budget_prunes_the_oldest_versions_across_projects() {
        let root = test_root("budget");
        let store = BackupStore::with_root(root.clone());
        // Six 100-byte versions spread over two projects.
        for index in 0..3u8 {
            let body = vec![b'a'; 100];
            store
                .record(&root.join("one.freya"), &body, 10)
                .expect("first project records");
            let body = vec![b'b'; 100];
            store
                .record(&root.join("two.freya"), &body, 10)
                .expect("second project records");
            let _ = index;
        }
        assert_eq!(store.total_size(), 600);
        let newest_before = store
            .projects()
            .into_iter()
            .flat_map(|project| project.entries.into_iter())
            .map(|entry| entry.file_name)
            .max()
            .expect("a newest version exists");
        // Room for four versions: the two oldest go, newest everywhere stays.
        let removed = store.enforce_total_budget(400).expect("budget prunes");
        assert_eq!(removed, 2);
        assert_eq!(store.total_size(), 400);
        let projects = store.projects();
        assert!(
            projects
                .iter()
                .flat_map(|project| project.entries.iter())
                .any(|entry| entry.file_name == newest_before),
            "the newest version from before pruning must survive it"
        );
        // Zero bytes still leaves one restorable copy.
        let removed = store.enforce_total_budget(0).expect("empty budget prunes");
        assert!(removed >= 1);
        assert_eq!(
            store.total_size(),
            100,
            "one newest version survives an impossible budget"
        );
        assert!(
            store
                .projects()
                .into_iter()
                .flat_map(|project| project.entries.into_iter())
                .any(|entry| entry.file_name == newest_before),
            "and it is still the same version"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_zero_limit_does_not_write_anything() {
        let root = test_root("disabled");
        let store = BackupStore::with_root(root.clone());
        let recorded = store
            .record(&root.join("project.freya"), b"version", 0)
            .expect("disabled store is not an error");
        assert!(recorded.is_none());
        assert!(!root.exists());
    }
}

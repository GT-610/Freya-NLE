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
        Self::with_root(application_data_root().join("backups"))
    }

    pub(crate) fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    /// The directory name that holds one project's backups.
    pub(crate) fn key_for(&self, project_path: &Path) -> String {
        self.project_dir(project_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The directory that holds one project's backups.
    fn project_dir(&self, project_path: &Path) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(project_path.to_string_lossy().as_bytes());
        let digest = hasher.finalize();
        let key = digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.root.join(key)
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
        if limit == 0 || bytes.len() as u64 > MAX_PROJECT_DOCUMENT_BYTES {
            return Ok(None);
        }
        let directory = self.project_dir(project_path);
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
        list_entries(&self.project_dir(project_path))
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
                if !valid_project_key(&key) {
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

    /// The project name recorded for a directory, when it has a manifest.
    pub(crate) fn project_label(&self, key: &str) -> Option<String> {
        if !valid_project_key(key) {
            return None;
        }
        read_manifest(&self.root.join(key)).map(|manifest| manifest.name)
    }

    /// Removes one version addressed by its directory key.
    pub(crate) fn remove_key(&self, key: &str, file_name: &str) -> Result<(), String> {
        let path = self.path_for_key(key, file_name)?;
        fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Resolves a directory key handed back by the UI. Both parts are checked
    /// here, so a forged key cannot reach outside the backup root.
    pub(crate) fn path_for_key(&self, key: &str, file_name: &str) -> Result<PathBuf, String> {
        if !valid_project_key(key) {
            return Err(format!("{key:?} is not a backup directory key"));
        }
        if !valid_backup_file(file_name) {
            return Err(format!("{file_name:?} is not a backup file name"));
        }
        Ok(self.root.join(key).join(file_name))
    }

    pub(crate) fn path_for(&self, project_path: &Path, file_name: &str) -> Result<PathBuf, String> {
        if !valid_backup_file(file_name) {
            return Err(format!("{file_name:?} is not a backup file name"));
        }
        Ok(self.project_dir(project_path).join(file_name))
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
        // Room for four versions: the two oldest go, newest everywhere stays.
        let removed = store.enforce_total_budget(400).expect("budget prunes");
        assert_eq!(removed, 2);
        assert_eq!(store.total_size(), 400);
        let projects = store.projects();
        let newest = projects
            .iter()
            .flat_map(|project| project.entries.iter())
            .map(|entry| entry.file_name.clone())
            .max()
            .expect("a newest version remains");
        assert!(
            projects
                .iter()
                .flat_map(|project| project.entries.iter())
                .any(|entry| entry.file_name == newest)
        );
        // Zero bytes still leaves one restorable copy.
        let removed = store.enforce_total_budget(0).expect("empty budget prunes");
        assert_eq!(removed, 3);
        assert_eq!(store.total_size(), 100);
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

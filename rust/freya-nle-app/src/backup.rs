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
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many saved versions one project keeps by default.
pub const DEFAULT_BACKUP_HISTORY_LIMIT: usize = 20;

const BACKUP_EXTENSION: &str = "freya";

/// One saved version on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    pub file_name: String,
    pub saved_at: String,
    pub size: u64,
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
        let directory = self.project_dir(project_path);
        let Ok(read_dir) = fs::read_dir(&directory) else {
            return Vec::new();
        };
        let mut entries = read_dir
            .flatten()
            .filter_map(|entry| {
                let file_name = entry.file_name().to_string_lossy().into_owned();
                if !valid_backup_file(&file_name) {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                let stamp = stamp_from_file(&file_name)?;
                Some(BackupEntry {
                    file_name,
                    saved_at: iso_timestamp(
                        SystemTime::UNIX_EPOCH
                            + Duration::from_millis(u64::try_from(stamp).unwrap_or(u64::MAX)),
                    ),
                    size: metadata.len(),
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| right.file_name.cmp(&left.file_name));
        entries
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
        let entries = self
            .entries_in(directory)
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

    fn entries_in(&self, directory: &Path) -> Vec<BackupEntry> {
        let Ok(read_dir) = fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut entries = read_dir
            .flatten()
            .filter_map(|entry| {
                let file_name = entry.file_name().to_string_lossy().into_owned();
                valid_backup_file(&file_name).then_some(BackupEntry {
                    file_name,
                    saved_at: String::new(),
                    size: entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| right.file_name.cmp(&left.file_name));
        entries
    }
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

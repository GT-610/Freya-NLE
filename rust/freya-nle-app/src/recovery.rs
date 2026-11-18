use crate::file_io::read_bounded;
use crate::project_io::write_atomic;
#[cfg(not(test))]
use crate::settings::application_data_root;
use freya_nle_core::api::{
    build_recovery_metadata, inspect_recovery_metadata, recovery_id_from_snapshot_name,
    recovery_id_is_valid, recovery_snapshot_name_is_valid,
};
use std::collections::{HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_BACKUP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_MAXIMUM_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
// Metadata holds only a few paths and labels, not the project document.
const MAX_RECOVERY_METADATA_BYTES: u64 = 1024 * 1024;
const INVALID_METADATA_PREFIX: &str = "invalid-metadata:";
static RECOVERY_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEntry {
    pub id: String,
    pub snapshot_path: Option<PathBuf>,
    pub original_project_url: String,
    pub display_name: String,
    pub saved_at: String,
    pub error: Option<String>,
}

impl RecoveryEntry {
    pub fn is_valid(&self) -> bool {
        self.error.is_none() && self.snapshot_path.is_some()
    }
}

fn invalid_metadata_filename(id: &str) -> Option<&str> {
    let name = id.strip_prefix(INVALID_METADATA_PREFIX)?;
    let stem = name.strip_suffix(".json")?;
    if stem.is_empty()
        || name.contains(['/', '\\', '\0'])
        || recovery_id_is_valid(stem)
        || Path::new(name).file_name()?.to_str()? != name
    {
        return None;
    }
    Some(name)
}

pub(crate) struct RecoveryWrite {
    pub id: String,
    pub original_project_url: String,
    pub display_name: String,
    pub snapshot: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryOperation {
    Write,
    Remove,
}

pub(crate) struct RecoveryEvent {
    pub id: String,
    pub operation: RecoveryOperation,
    pub result: Result<String, String>,
}

enum RecoveryCommand {
    Write(RecoveryWrite),
    Remove(String),
    #[cfg(test)]
    Barrier(Sender<()>),
    Shutdown,
}

pub(crate) struct RecoveryStore {
    root: PathBuf,
    sender: Option<Sender<RecoveryCommand>>,
    result_receiver: Receiver<RecoveryEvent>,
    worker: Option<JoinHandle<()>>,
}

impl RecoveryStore {
    #[cfg(not(test))]
    pub(crate) fn new() -> Self {
        Self::with_root(default_recovery_root())
    }

    pub(crate) fn with_root(root: PathBuf) -> Self {
        let _ = cleanup_stale(&root, SystemTime::now(), DEFAULT_MAXIMUM_AGE);
        let (sender, receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let worker_root = root.clone();
        let worker = thread::Builder::new()
            .name("freya-nle-project-recovery".to_owned())
            .spawn(move || run_worker(worker_root, receiver, result_sender))
            .expect("project recovery worker must start");
        Self {
            root,
            sender: Some(sender),
            result_receiver,
            worker: Some(worker),
        }
    }

    pub(crate) fn request_write(&self, request: RecoveryWrite) -> Result<(), String> {
        self.sender
            .as_ref()
            .ok_or_else(|| "project recovery worker is unavailable".to_owned())?
            .send(RecoveryCommand::Write(request))
            .map_err(|_| "project recovery worker stopped".to_owned())
    }

    pub(crate) fn request_remove(&self, id: &str) -> Result<(), String> {
        if !recovery_id_is_valid(id)
            && recovery_id_from_snapshot_name(id).is_none()
            && invalid_metadata_filename(id).is_none()
        {
            return Err("invalid recovery identifier".to_owned());
        }
        self.sender
            .as_ref()
            .ok_or_else(|| "project recovery worker is unavailable".to_owned())?
            .send(RecoveryCommand::Remove(id.to_owned()))
            .map_err(|_| "project recovery worker stopped".to_owned())
    }

    pub(crate) fn entries(&self) -> Vec<RecoveryEntry> {
        scan_recoveries(&self.root)
    }

    pub(crate) fn poll(&self) -> Vec<RecoveryEvent> {
        self.result_receiver.try_iter().collect()
    }

    #[cfg(test)]
    pub(crate) fn flush(&self) {
        let (sender, receiver) = mpsc::channel();
        self.sender
            .as_ref()
            .expect("project recovery worker is available")
            .send(RecoveryCommand::Barrier(sender))
            .expect("project recovery worker accepts barrier");
        receiver
            .recv()
            .expect("project recovery worker reaches barrier");
    }
}

impl Drop for RecoveryStore {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(RecoveryCommand::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) fn generate_recovery_id() -> String {
    let count = RECOVERY_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut first = DefaultHasher::new();
    nanos.hash(&mut first);
    count.hash(&mut first);
    std::process::id().hash(&mut first);
    let high = first.finish();
    let mut second = DefaultHasher::new();
    high.hash(&mut second);
    nanos.rotate_left(37).hash(&mut second);
    count.rotate_left(17).hash(&mut second);
    let low = second.finish();
    let value = (u128::from(high) << 64) | u128::from(low.max(1));
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (value >> 96) as u32,
        (value >> 80) as u16,
        (value >> 64) as u16,
        (value >> 48) as u16,
        value & 0x0000_ffff_ffff_ffff
    )
}

fn run_worker(
    root: PathBuf,
    receiver: Receiver<RecoveryCommand>,
    result_sender: Sender<RecoveryEvent>,
) {
    while let Ok(command) = receiver.recv() {
        let (id, operation, result) = match command {
            RecoveryCommand::Write(request) => {
                let id = request.id.clone();
                let result = write_recovery(&root, &request)
                    .map(|()| format!("Recovery snapshot updated for {}", request.display_name));
                (id, RecoveryOperation::Write, result)
            }
            RecoveryCommand::Remove(id) => {
                let result =
                    remove_recovery(&root, &id).map(|()| "Recovery snapshot discarded".to_owned());
                (id, RecoveryOperation::Remove, result)
            }
            #[cfg(test)]
            RecoveryCommand::Barrier(sender) => {
                let _ = sender.send(());
                continue;
            }
            RecoveryCommand::Shutdown => break,
        };
        let _ = result_sender.send(RecoveryEvent {
            id,
            operation,
            result: result.map_err(|error| error.to_string()),
        });
    }
}

fn write_recovery(root: &Path, request: &RecoveryWrite) -> io::Result<()> {
    if !recovery_id_is_valid(&request.id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid recovery identifier",
        ));
    }
    fs::create_dir_all(root)?;
    let generation = generate_recovery_id();
    let snapshot_file = format!("{}-{generation}.aviqtl", request.id);
    if !recovery_snapshot_name_is_valid(&request.id, &snapshot_file) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid recovery snapshot name",
        ));
    }
    let previous_snapshot = read_current_snapshot_name(root, &request.id);
    let metadata = build_recovery_metadata(
        &request.id,
        &request.original_project_url,
        &request.display_name,
        &iso_timestamp(SystemTime::now()),
        &snapshot_file,
    )
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid recovery metadata"))?;
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if metadata_bytes.len() as u64 > MAX_RECOVERY_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recovery metadata exceeds the size limit",
        ));
    }
    write_atomic(&root.join(&snapshot_file), &request.snapshot)?;
    if let Err(error) = write_atomic(&root.join(format!("{}.json", request.id)), &metadata_bytes) {
        let _ = fs::remove_file(root.join(&snapshot_file));
        return Err(error);
    }
    if let Some(previous) = previous_snapshot.filter(|previous| previous != &snapshot_file) {
        let _ = fs::remove_file(root.join(previous));
    }
    Ok(())
}

fn read_current_snapshot_name(root: &Path, id: &str) -> Option<String> {
    let metadata = read_bounded(
        &root.join(format!("{id}.json")),
        MAX_RECOVERY_METADATA_BYTES,
    )
    .ok()?;
    let inspection = inspect_recovery_metadata(id, &metadata)?;
    (inspection.status == "ok"
        && recovery_snapshot_name_is_valid(id, &inspection.metadata.snapshot_file))
    .then_some(inspection.metadata.snapshot_file)
}

fn remove_recovery(root: &Path, id: &str) -> io::Result<()> {
    if let Some(name) = invalid_metadata_filename(id) {
        return match fs::remove_file(root.join(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    }
    if let Some(owner) = recovery_id_from_snapshot_name(id) {
        if read_current_snapshot_name(root, &owner).as_deref() == Some(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot is referenced by recovery metadata",
            ));
        }
        let result = match fs::remove_file(root.join(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        remove_generation_origin(root, id);
        return result;
    }
    if !recovery_id_is_valid(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid recovery identifier",
        ));
    }
    let current = read_current_snapshot_name(root, id);
    // Newer unreferenced generations outlive this metadata, so record where the
    // project came from before the metadata that holds that answer is deleted.
    preserve_surviving_generations(root, id, current.as_deref());
    if let Some(snapshot) = current {
        match fs::remove_file(root.join(&snapshot)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        remove_generation_origin(root, &snapshot);
    }
    let metadata = root.join(format!("{id}.json"));
    match fs::remove_file(metadata) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Where a surviving generation's provenance is kept once its owner metadata goes.
///
/// Discarding a recovery removes `<id>.json` but keeps newer unreferenced
/// generations, which would then lose the original project URL the snapshot's
/// relative media paths resolve against. The sidecar records it next to the
/// snapshot. The extension is deliberately not `.json` and not a snapshot name,
/// so both scans ignore it as an entry of its own.
const GENERATION_ORIGIN_EXTENSION: &str = "origin";

fn generation_origin_path(root: &Path, snapshot_file: &str) -> PathBuf {
    root.join(format!("{snapshot_file}.{GENERATION_ORIGIN_EXTENSION}"))
}

fn write_generation_origin(
    root: &Path,
    snapshot_file: &str,
    original_project_url: &str,
    display_name: &str,
) -> io::Result<()> {
    if recovery_id_from_snapshot_name(snapshot_file).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid recovery snapshot name",
        ));
    }
    let document = serde_json::json!({
        "version": 1,
        "originalProjectUrl": original_project_url,
        "displayName": display_name,
    });
    let bytes = serde_json::to_vec(&document)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() as u64 > MAX_RECOVERY_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recovery origin exceeds the size limit",
        ));
    }
    write_atomic(&generation_origin_path(root, snapshot_file), &bytes)
}

/// Reads the provenance kept for one surviving generation, if any.
fn read_generation_origin(root: &Path, snapshot_file: &str) -> Option<(String, String)> {
    // Only a name this module wrote can address a sidecar.
    recovery_id_from_snapshot_name(snapshot_file)?;
    let bytes = read_bounded(
        &generation_origin_path(root, snapshot_file),
        MAX_RECOVERY_METADATA_BYTES,
    )
    .ok()?;
    let document: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let object = document.as_object()?;
    let url = object
        .get("originalProjectUrl")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let name = object
        .get("displayName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some((url, name))
}

fn remove_generation_origin(root: &Path, snapshot_file: &str) {
    let _ = fs::remove_file(generation_origin_path(root, snapshot_file));
}

/// Records the owner's provenance on every generation that will outlive its
/// metadata, so discarding one recovery does not orphan the others.
fn preserve_surviving_generations(root: &Path, owner: &str, current: Option<&str>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let metadata = read_bounded(
        &root.join(format!("{owner}.json")),
        MAX_RECOVERY_METADATA_BYTES,
    )
    .ok()
    .and_then(|bytes| inspect_recovery_metadata(owner, &bytes))
    .filter(|inspection| inspection.status == "ok");
    let Some(inspection) = metadata else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if Some(name.as_str()) == current
            || recovery_id_from_snapshot_name(&name).as_deref() != Some(owner)
        {
            continue;
        }
        if generation_origin_path(root, &name).exists() {
            continue;
        }
        // A failed write only means this generation keeps the old behaviour, so
        // it must not stop the discard the user asked for.
        let _ = write_generation_origin(
            root,
            &name,
            &inspection.metadata.original_project_url,
            &inspection.metadata.display_name,
        );
    }
}

fn scan_recoveries(root: &Path) -> Vec<RecoveryEntry> {
    let mut recoveries = scan_recovery_metadata(root);
    let referenced: HashSet<PathBuf> = recoveries
        .iter()
        .filter_map(|entry| entry.snapshot_path.clone())
        .collect();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(owner) = recovery_id_from_snapshot_name(name) else {
                continue;
            };
            if referenced.contains(&path) {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let parent = recoveries
                .iter()
                .find(|entry| entry.id == owner && entry.is_valid());
            // The owner metadata may be gone because the user discarded that
            // recovery while this generation survived; its provenance was kept
            // beside the snapshot, so the project's media directory still
            // resolves.
            let recorded = parent
                .is_none()
                .then(|| read_generation_origin(root, name))
                .flatten();
            let display_name = match (parent, &recorded) {
                (Some(entry), _) => format!("{} (interrupted recovery)", entry.display_name),
                (None, Some((_, name))) if !name.is_empty() => {
                    format!("{name} (interrupted recovery)")
                }
                _ => "Interrupted recovery".to_owned(),
            };
            recoveries.push(RecoveryEntry {
                id: name.to_owned(),
                snapshot_path: Some(path.clone()),
                original_project_url: parent.map_or_else(
                    || recorded.map(|(url, _)| url).unwrap_or_default(),
                    |entry| entry.original_project_url.clone(),
                ),
                display_name,
                saved_at: metadata.modified().map(iso_timestamp).unwrap_or_default(),
                error: None,
            });
        }
    }
    recoveries.sort_by(|left, right| right.saved_at.cmp(&left.saved_at));
    recoveries
}

fn scan_recovery_metadata(root: &Path) -> Vec<RecoveryEntry> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut recoveries = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let id = path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut recovery = RecoveryEntry {
            id: id.clone(),
            snapshot_path: None,
            original_project_url: String::new(),
            display_name: id.clone(),
            saved_at: String::new(),
            error: None,
        };
        if !recovery_id_is_valid(&id) {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let discard_id = format!("{INVALID_METADATA_PREFIX}{name}");
            if invalid_metadata_filename(&discard_id).is_none() {
                continue;
            }
            recovery.id = discard_id;
            recovery.error = Some("Recovery identifier is invalid".to_owned());
            recoveries.push(recovery);
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            recovery.error = Some("Recovery metadata must not be a symbolic link".to_owned());
            recoveries.push(recovery);
            continue;
        }
        let inspection = read_bounded(&path, MAX_RECOVERY_METADATA_BYTES)
            .ok()
            .and_then(|bytes| inspect_recovery_metadata(&id, &bytes));
        let Some(inspection) = inspection else {
            recovery.error = Some("Recovery metadata must be a valid JSON object".to_owned());
            recoveries.push(recovery);
            continue;
        };
        recovery.original_project_url = inspection.metadata.original_project_url;
        recovery.display_name = inspection.metadata.display_name;
        recovery.saved_at = inspection.metadata.saved_at;
        if inspection.status != "ok" {
            recovery.error = Some(format!("Recovery metadata status: {}", inspection.status));
            recoveries.push(recovery);
            continue;
        }
        let snapshot_path = root.join(&inspection.metadata.snapshot_file);
        match fs::symlink_metadata(&snapshot_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                recovery.error = Some("Recovery snapshot must not be a symbolic link".to_owned());
            }
            Ok(metadata) if metadata.is_file() => recovery.snapshot_path = Some(snapshot_path),
            Ok(_) => recovery.error = Some("Recovery snapshot is not a file".to_owned()),
            Err(error) => recovery.error = Some(format!("Recovery snapshot is missing: {error}")),
        }
        recoveries.push(recovery);
    }
    recoveries.sort_by(|left, right| right.saved_at.cmp(&left.saved_at));
    recoveries
}

fn cleanup_stale(root: &Path, now: SystemTime, maximum_age: Duration) -> io::Result<()> {
    let cutoff = now.checked_sub(maximum_age).unwrap_or(UNIX_EPOCH);
    let mut referenced_snapshots = HashSet::new();
    for recovery in scan_recovery_metadata(root) {
        if !recovery_id_is_valid(&recovery.id) {
            continue;
        }
        let metadata_path = root.join(format!("{}.json", recovery.id));
        let is_stale = fs::symlink_metadata(&metadata_path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < cutoff);
        if is_stale {
            // A crash can leave a newer, unreferenced generation next to old
            // metadata. Remove only the metadata here; each snapshot is aged
            // independently below, and the surviving generations keep the
            // provenance the metadata used to hold.
            preserve_surviving_generations(
                root,
                &recovery.id,
                recovery
                    .snapshot_path
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str()),
            );
            match fs::remove_file(&metadata_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        } else if let Some(snapshot_path) = recovery.snapshot_path {
            referenced_snapshots.insert(snapshot_path);
        }
    }

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if recovery_id_from_snapshot_name(file_name).is_none()
            || referenced_snapshots.contains(&path)
        {
            continue;
        }
        let is_stale = fs::symlink_metadata(&path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < cutoff);
        if is_stale {
            fs::remove_file(&path)?;
            remove_generation_origin(root, file_name);
        }
    }
    Ok(())
}

#[cfg(not(test))]
fn default_recovery_root() -> PathBuf {
    application_data_root().join("recovery")
}

pub(crate) fn iso_timestamp(time: SystemTime) -> String {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = i64::try_from(duration.as_secs()).unwrap_or(i64::MAX);
    let days = seconds / 86_400;
    let second_of_day = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    let hour = second_of_day / 3_600;
    let minute = second_of_day % 3_600 / 60;
    let second = second_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        duration.subsec_millis()
    )
}

fn civil_date(days_since_epoch: i64) -> (i64, i64, i64) {
    let days = days_since_epoch + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_io::ProjectSession;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "freya-nle-app-recovery-{name}-{}-{}",
            std::process::id(),
            RECOVERY_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Opens an existing fixture so its timestamp can be changed.
    ///
    /// `File::open` asks for read access only, and Windows refuses `SetFileTime`
    /// through a read-only handle, so a fixture that is backdated needs write
    /// access. Nothing is truncated, so the file keeps its contents.
    fn aged_fixture(path: &Path) -> fs::File {
        fs::OpenOptions::new()
            .write(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }

    fn recovery_write(id: String, scene_name: &str) -> RecoveryWrite {
        let project = ProjectSession::blank();
        let mut snapshot: serde_json::Value =
            serde_json::from_slice(&project.snapshot_bytes().expect("snapshot serializes"))
                .expect("snapshot parses");
        snapshot["scenes"][0]["name"] = serde_json::Value::String(scene_name.to_owned());
        RecoveryWrite {
            id,
            original_project_url: "/tmp/original.aviqtl".to_owned(),
            display_name: "Original".to_owned(),
            snapshot: serde_json::to_vec_pretty(&snapshot).expect("snapshot serializes"),
        }
    }

    #[test]
    fn generated_ids_and_timestamps_follow_the_rust_core_contract() {
        let id = generate_recovery_id();
        assert!(recovery_id_is_valid(&id));
        assert_eq!(
            iso_timestamp(UNIX_EPOCH + Duration::from_secs(946_684_800)),
            "2000-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn queued_writes_replace_the_previous_generation() {
        let root = test_root("replace");
        let store = RecoveryStore::with_root(root.clone());
        let id = generate_recovery_id();
        store
            .request_write(recovery_write(id.clone(), "First"))
            .expect("first recovery queues");
        store
            .request_write(recovery_write(id.clone(), "Second"))
            .expect("replacement recovery queues");
        store.flush();

        let entries = store.entries();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_valid());
        assert_eq!(entries[0].id, id);
        let recovered = ProjectSession::load_recovery(
            entries[0]
                .snapshot_path
                .as_deref()
                .expect("snapshot path exists"),
            &entries[0].original_project_url,
        )
        .expect("latest snapshot recovers");
        assert_eq!(recovered.document.scenes[0].name, "Second");
        assert_eq!(
            fs::read_dir(&root)
                .expect("recovery root exists")
                .flatten()
                .filter(|entry| entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "aviqtl"))
                .count(),
            1
        );

        drop(store);
        fs::remove_dir_all(root).expect("test recovery root removes");
    }

    #[test]
    fn stale_cleanup_removes_expired_recoveries_and_orphaned_snapshots() {
        let root = test_root("cleanup");
        let id = generate_recovery_id();
        write_recovery(&root, &recovery_write(id, "Root")).expect("recovery writes");
        let orphan_id = generate_recovery_id();
        let orphan_generation = generate_recovery_id();
        let orphan_path = root.join(format!("{orphan_id}-{orphan_generation}.aviqtl"));
        fs::write(&orphan_path, b"orphan").expect("orphan snapshot writes");

        cleanup_stale(&root, SystemTime::now(), DEFAULT_MAXIMUM_AGE)
            .expect("recent recoveries remain");
        assert_eq!(scan_recoveries(&root).len(), 2);
        assert!(orphan_path.exists());

        let future = SystemTime::now()
            .checked_add(DEFAULT_MAXIMUM_AGE + Duration::from_secs(1))
            .expect("test timestamp fits");
        cleanup_stale(&root, future, DEFAULT_MAXIMUM_AGE).expect("expired recoveries clean up");
        assert!(scan_recoveries(&root).is_empty());
        assert!(!orphan_path.exists());
        fs::remove_dir_all(root).expect("test recovery root removes");
    }

    #[test]
    fn stale_metadata_does_not_delete_a_newer_interrupted_snapshot() {
        let root = test_root("interrupted-generation");
        let id = generate_recovery_id();
        write_recovery(&root, &recovery_write(id.clone(), "Old")).unwrap();
        let old_snapshot = root.join(read_current_snapshot_name(&root, &id).unwrap());
        let metadata = root.join(format!("{id}.json"));
        let old_time = SystemTime::now() - DEFAULT_MAXIMUM_AGE - Duration::from_secs(60);
        for path in [&metadata, &old_snapshot] {
            // `File::open` is read-only; Windows refuses `SetFileTime` on that
            // handle, so ask for write access. The file keeps its contents.
            aged_fixture(path)
                .set_times(fs::FileTimes::new().set_modified(old_time))
                .unwrap();
        }
        let new_snapshot = root.join(format!("{}-{}.aviqtl", id, generate_recovery_id()));
        fs::write(&new_snapshot, b"new snapshot").unwrap();

        cleanup_stale(&root, SystemTime::now(), DEFAULT_MAXIMUM_AGE).unwrap();
        assert!(!metadata.exists());
        assert!(!old_snapshot.exists());
        assert_eq!(fs::read(&new_snapshot).unwrap(), b"new snapshot");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discarding_one_recovery_keeps_the_others_media_directory() {
        // Discarding the current recovery deletes its metadata but keeps a newer
        // generation that a crash left behind. Without the provenance recorded
        // beside that snapshot, the next scan has no original project URL, and
        // the recovered project cannot resolve its relative media paths.
        let root = test_root("discard-keeps-origin");
        let id = generate_recovery_id();
        write_recovery(&root, &recovery_write(id.clone(), "Old")).unwrap();
        let generated = root.join(format!("{}-{}.aviqtl", id, generate_recovery_id()));
        fs::write(&generated, b"newer generation").unwrap();

        remove_recovery(&root, &id).expect("the recovery is discarded");
        assert!(!root.join(format!("{id}.json")).exists());
        assert!(generated.exists(), "the newer generation survives");

        let entries = scan_recoveries(&root);
        let orphan = entries
            .iter()
            .find(|entry| entry.id.ends_with(".aviqtl"))
            .expect("the surviving generation is listed");
        assert_eq!(
            orphan.original_project_url, "/tmp/original.aviqtl",
            "the discard must not lose the project's directory"
        );
        assert!(
            orphan.display_name.contains("Original"),
            "the name survives too: {}",
            orphan.display_name
        );

        // Discarding the generation cleans the sidecar up with it.
        remove_recovery(&root, &orphan.id).expect("the generation is discarded");
        assert!(
            !generation_origin_path(&root, generated.file_name().unwrap().to_str().unwrap())
                .exists()
        );
        assert!(scan_recoveries(&root).is_empty());
        fs::remove_dir_all(root).expect("test recovery root removes");
    }

    #[test]
    fn stale_cleanup_keeps_the_origin_of_a_surviving_generation() {
        // The same loss happens when the metadata ages out rather than being
        // discarded, so the cleanup path records the provenance as well.
        let root = test_root("cleanup-keeps-origin");
        let id = generate_recovery_id();
        write_recovery(&root, &recovery_write(id.clone(), "Old")).unwrap();
        let metadata = root.join(format!("{id}.json"));
        let old_time = SystemTime::now() - DEFAULT_MAXIMUM_AGE - Duration::from_secs(60);
        aged_fixture(&metadata)
            .set_times(fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        let generated = root.join(format!("{}-{}.aviqtl", id, generate_recovery_id()));
        fs::write(&generated, b"newer generation").unwrap();

        cleanup_stale(&root, SystemTime::now(), DEFAULT_MAXIMUM_AGE).expect("stale cleanup runs");
        assert!(!metadata.exists());

        let entries = scan_recoveries(&root);
        let orphan = entries
            .iter()
            .find(|entry| entry.id.ends_with(".aviqtl"))
            .expect("the surviving generation is listed");
        assert_eq!(orphan.original_project_url, "/tmp/original.aviqtl");
        fs::remove_dir_all(root).expect("test recovery root removes");
    }

    #[test]
    fn invalid_identifiers_and_path_escape_metadata_are_never_recoverable() {
        let root = test_root("invalid");
        fs::create_dir_all(&root).expect("test recovery root creates");
        let outside = root.with_extension("outside.aviqtl");
        fs::write(&outside, b"outside").expect("outside fixture writes");
        fs::write(root.join("invalid.json"), b"{}").expect("invalid metadata writes");
        let id = generate_recovery_id();
        let metadata = serde_json::json!({
            "version": 1,
            "id": id,
            "originalProjectUrl": "",
            "displayName": "Escape",
            "savedAt": "2026-09-06T00:00:00.000Z",
            "snapshotFile": "../outside.aviqtl"
        });
        fs::write(
            root.join(format!("{id}.json")),
            serde_json::to_vec(&metadata).expect("metadata serializes"),
        )
        .expect("escape metadata writes");

        let entries = scan_recoveries(&root);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| !entry.is_valid()));
        assert_eq!(
            fs::read(&outside).expect("outside fixture remains"),
            b"outside"
        );
        remove_recovery(&root, &id).expect("valid id metadata removes safely");
        assert_eq!(
            fs::read(&outside).expect("outside fixture remains"),
            b"outside"
        );

        fs::remove_file(outside).expect("outside fixture removes");
        fs::remove_dir_all(root).expect("test recovery root removes");
    }

    #[test]
    fn oversized_metadata_is_rejected_without_replacing_a_valid_snapshot() {
        let root = test_root("oversized");
        let id = generate_recovery_id();
        write_recovery(&root, &recovery_write(id.clone(), "First")).unwrap();
        let before = scan_recoveries(&root)[0].snapshot_path.clone();
        let mut request = recovery_write(id.clone(), "Second");
        request.display_name = "x".repeat(MAX_RECOVERY_METADATA_BYTES as usize);
        assert_eq!(
            write_recovery(&root, &request).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(scan_recoveries(&root)[0].snapshot_path, before);
        fs::write(
            root.join(format!("{id}.json")),
            vec![b'x'; MAX_RECOVERY_METADATA_BYTES as usize + 1],
        )
        .unwrap();
        let entries = scan_recoveries(&root);
        assert!(
            entries
                .iter()
                .any(|entry| entry.id == id && !entry.is_valid())
        );
        assert!(entries.iter().any(|entry| entry.is_valid()));
        assert!(read_current_snapshot_name(&root, &id).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn fifo_metadata_is_rejected_without_opening_it() {
        let root = test_root("fifo");
        fs::create_dir_all(&root).unwrap();
        let id = generate_recovery_id();
        let path = root.join(format!("{id}.json"));
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(!scan_recoveries(&root)[0].is_valid());
        assert!(read_current_snapshot_name(&root, &id).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn orphan_symlinks_are_not_listed_and_referenced_snapshots_cannot_be_discarded_as_orphans() {
        use std::os::unix::fs::symlink;
        let root = test_root("orphan-symlink");
        let id = generate_recovery_id();
        write_recovery(&root, &recovery_write(id.clone(), "Original")).unwrap();
        let snapshot = read_current_snapshot_name(&root, &id).unwrap();
        let link = root.join(format!("{id}-{}.aviqtl", generate_recovery_id()));
        symlink(root.join(&snapshot), &link).unwrap();
        assert_eq!(scan_recoveries(&root).len(), 1);
        assert!(remove_recovery(&root, &snapshot).is_err());
        assert!(root.join(snapshot).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_metadata_and_snapshots_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink");
        fs::create_dir_all(&root).expect("test recovery root creates");
        let target = root.with_extension("symlink-target");
        fs::write(&target, b"{}").expect("target writes");
        let metadata_id = generate_recovery_id();
        symlink(&target, root.join(format!("{metadata_id}.json")))
            .expect("metadata symlink creates");

        let snapshot_id = generate_recovery_id();
        let snapshot_name = format!("{snapshot_id}-{}.aviqtl", generate_recovery_id());
        symlink(&target, root.join(&snapshot_name)).expect("snapshot symlink creates");
        let metadata = build_recovery_metadata(
            &snapshot_id,
            "",
            "Snapshot symlink",
            "2026-09-06T00:00:00.000Z",
            &snapshot_name,
        )
        .expect("metadata is valid");
        fs::write(
            root.join(format!("{snapshot_id}.json")),
            serde_json::to_vec(&metadata).expect("metadata serializes"),
        )
        .expect("metadata writes");

        let entries = scan_recoveries(&root);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| !entry.is_valid()));
        assert!(entries.iter().any(|entry| {
            entry
                .error
                .as_deref()
                .is_some_and(|error| error.contains("symbolic link"))
        }));
        remove_recovery(&root, &metadata_id).expect("metadata symlink removes");
        remove_recovery(&root, &snapshot_id).expect("snapshot symlink removes");
        assert_eq!(fs::read(&target).expect("symlink target remains"), b"{}");

        fs::remove_file(target).expect("symlink target removes");
        fs::remove_dir_all(root).expect("test recovery root removes");
    }
}

//! Process-wide logging: stderr, an optional rotating file, the in-memory ring
//! the log window reads, `RUST_LOG` filtering, and a panic record.
//!
//! The binary installs this once, before anything else runs. Library crates
//! only use the `log` facade and never install a logger, so tests stay quiet
//! and the sinks remain a single decision made here.
//!
//! File logging is off by default: a launch should not leave a file behind
//! unless the user asked for one (Preferences → Log) or `FREYA_NLE_LOG_DIR`
//! names a directory for scripted runs. Recent records always live in memory
//! for the log window, which is what AviUtl2's log window exposes.
//!
//! stdout is reserved for machine-readable protocols (`--validate-gpu`'s JSON,
//! `--help`, `--version`). Everything diagnostic goes to stderr and the file.

use log::Level;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The timestamp every sink writes; the log window drops the date, since a
/// session rarely crosses midnight and the window shows only this run.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3f%:z";

/// Directory under the application data root that holds log files.
const LOG_DIRECTORY: &str = "logs";
const LOG_FILE_NAME: &str = "freya-nle.log";
/// `freya-nle.log` is rotated to `.1` when it would exceed this size.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// How many rotated files are kept beside the current one.
const KEPT_LOG_FILES: u32 = 3;
/// How far back the log window can scroll.
const MAX_RECENT_ENTRIES: usize = 500;

/// `RUST_LOG` wins when set. The default keeps the application at `info` and
/// silences third-party stacks that log per-frame or per-widget at `debug`.
const DEFAULT_FILTER: &str =
    "info,wgpu=warn,naga=warn,winit=warn,calloop=warn,softbuffer=warn,zbus=warn";

/// Recent records, oldest first. Shared by every thread that logs.
static RECENT: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());
/// Bumped for every recorded line so the log window can poll cheaply.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Whether records are copied into the rotating file. See the module docs.
static FILE_ENABLED: AtomicBool = AtomicBool::new(false);
/// Bumped whenever the switch changes. A new epoch is a fresh attempt, so
/// re-enabling retries an open that failed earlier.
static SWITCH_EPOCH: AtomicU64 = AtomicU64::new(0);
/// The open file, the switch epoch it reflects, and the last failure.
///
/// One lock covers all three, so a toggle and a write cannot disagree about
/// whether the file is open, and the window reads the state the writer acts on.
static FILE_SINK: Mutex<FileSink> = Mutex::new(FileSink {
    file: None,
    epoch: 0,
    gave_up: false,
    error: None,
});

struct FileSink {
    file: Option<RotatingFile>,
    /// The switch epoch the fields above reflect.
    epoch: u64,
    /// Set when the file could not be opened, so an unwritable data directory
    /// is not retried on every record. A toggle clears it.
    gave_up: bool,
    /// Why the last attempt failed, for the log window.
    error: Option<String>,
}

/// One line as the log window and saved transcripts show it.
#[derive(Clone)]
pub(super) struct Entry {
    level: Level,
    time: String,
    thread: String,
    target: String,
    message: String,
}

impl Entry {
    pub(super) fn level(&self) -> Level {
        self.level
    }

    /// The single-line form shared by the window and a saved transcript.
    pub(super) fn line(&self) -> String {
        format!(
            "{} {:<5} [{}] {}: {}",
            self.time, self.level, self.thread, self.target, self.message
        )
    }
}

/// Installs the global logger, unless one is already installed.
///
/// Returns whether this call installed it. stderr is always written; the file
/// appears only once `set_file_enabled` turns it on.
pub(super) fn init() -> bool {
    FILE_ENABLED.store(
        std::env::var_os("FREYA_NLE_LOG_DIR").is_some(),
        Ordering::Relaxed,
    );
    let filter = std::env::var("RUST_LOG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_FILTER.to_owned());
    let target = env_logger::Target::Pipe(Box::new(TeeWriter::new()));
    let mut builder = env_logger::Builder::new();
    builder
        .parse_filters(&filter)
        .target(target)
        .format(format_record);
    let logger = builder.build();
    let level = logger.filter();
    // One logger lives for the process; leaking it keeps `set_logger`'s
    // `'static` requirement without an extra allocation every call.
    let recorder = Box::leak(Box::new(RecordingLogger { inner: logger }));
    if log::set_logger(recorder).is_err() {
        return false;
    }
    log::set_max_level(level);
    install_panic_hook();
    if FILE_ENABLED.load(Ordering::Relaxed) {
        announce_file();
    }
    true
}

/// Turns the rotating file on or off at runtime and records the transition.
pub(super) fn set_file_enabled(enabled: bool) {
    if enabled {
        // A new epoch is a fresh attempt: the sink forgets the previous
        // failure and retries, even when no record arrived while it was off.
        SWITCH_EPOCH.fetch_add(1, Ordering::Relaxed);
        FILE_ENABLED.store(true, Ordering::Relaxed);
        announce_file();
    } else {
        // Recorded while the file is still open, so the file itself ends with
        // the reason; the switch flips and closes it below.
        log::info!(target: "app", "log file disabled");
        FILE_ENABLED.store(false, Ordering::Relaxed);
        sync_file();
    }
}

/// Opens the file, names it in a record, and reports a failure.
///
/// Opening is this function's job rather than the record's: at `RUST_LOG=warn`
/// the `info` below is filtered out, and the file still has to exist. The
/// warning is a record, so a terminal launch learns the reason too; the window
/// reads the same state through [`file_error`].
fn announce_file() {
    sync_file();
    log::info!(target: "app", "log file: {}", file_path().display());
    if let Some(error) = file_error() {
        log::warn!(target: "app", "could not write the log file: {error}");
    }
}

fn lock_sink() -> std::sync::MutexGuard<'static, FileSink> {
    FILE_SINK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Opens or closes the file to match the current switch.
fn sync_file() {
    let mut sink = lock_sink();
    sync_locked(&mut sink);
}

fn sync_locked(sink: &mut FileSink) {
    reconcile(
        sink,
        SWITCH_EPOCH.load(Ordering::Relaxed),
        FILE_ENABLED.load(Ordering::Relaxed),
        &log_directory(),
    );
}

/// Records a failure next to the switch state the window reads.
fn note_file_error(sink: &mut FileSink, error: io::Error) {
    sink.error = Some(format!("{}: {error}", file_path().display()));
}

/// Brings one sink in line with the switch. Split out so a test can drive the
/// transitions without the process-wide state or a real data directory.
fn reconcile(sink: &mut FileSink, epoch: u64, enabled: bool, directory: &Path) {
    if sink.epoch != epoch {
        sink.epoch = epoch;
        // The user asked again: reopen and report afresh.
        sink.file = None;
        sink.gave_up = false;
    }
    if !enabled {
        sink.file = None;
        sink.error = None;
        return;
    }
    if sink.file.is_some() || sink.gave_up {
        return;
    }
    let path = directory.join(LOG_FILE_NAME);
    match RotatingFile::open(path.clone()) {
        Ok(file) => {
            sink.file = Some(file);
            sink.error = None;
        }
        Err(error) => {
            sink.gave_up = true;
            sink.error = Some(format!("{}: {error}", path.display()));
        }
    }
}

/// Appends one record. A failure is kept for the window, not logged from
/// here: this path already holds the sink lock.
fn write_file(buf: &[u8]) {
    let mut sink = lock_sink();
    sync_locked(&mut sink);
    let Some(file) = &mut sink.file else {
        return;
    };
    match file.write(buf) {
        Ok(()) => sink.error = None,
        Err(error) => note_file_error(&mut sink, error),
    }
}

fn flush_file() {
    let mut sink = lock_sink();
    if let Some(file) = &mut sink.file
        && let Err(error) = file.flush()
    {
        note_file_error(&mut sink, error);
    }
}

pub(super) fn file_enabled() -> bool {
    FILE_ENABLED.load(Ordering::Relaxed)
}

/// Why the file could not be opened, or `None` while it is working.
pub(super) fn file_error() -> Option<String> {
    FILE_SINK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .error
        .clone()
}

/// The directory logs are written to: `FREYA_NLE_LOG_DIR` for scripted runs,
/// otherwise `logs/` under the application data root.
pub(super) fn log_directory() -> PathBuf {
    if let Some(directory) = std::env::var_os("FREYA_NLE_LOG_DIR") {
        return PathBuf::from(directory);
    }
    freya_nle_app::settings::application_data_root().join(LOG_DIRECTORY)
}

pub(super) fn file_path() -> PathBuf {
    log_directory().join(LOG_FILE_NAME)
}

/// The recent records, oldest first, with the generation they belong to.
///
/// Both come from one lock: reading them separately would let a record that
/// arrived mid-draw be marked as painted without ever being shown.
pub(super) fn snapshot() -> (Vec<Entry>, u64) {
    let recent = RECENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (
        recent.iter().cloned().collect(),
        GENERATION.load(Ordering::Relaxed),
    )
}

pub(super) fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Forgets the in-memory records behind the log window; the file, if open,
/// keeps its history.
pub(super) fn clear_recent() {
    let mut recent = RECENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    recent.clear();
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The build and host a pasted log came from, so it stays self-describing.
fn build_stamp() -> String {
    format!(
        "Freya NLE {} ({}, {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// The moment a transcript or a file header was started.
fn opened_at() -> String {
    chrono::Local::now().format(TIMESTAMP_FORMAT).to_string()
}

/// The recent records as one text document for a saved transcript.
pub(super) fn transcript() -> String {
    let mut text = format!("{} log transcript at {}\n", build_stamp(), opened_at());
    for entry in snapshot().0 {
        text.push_str(&entry.line());
        text.push('\n');
    }
    text
}

/// Wraps the filtered sink so every record it accepts also fills the ring.
struct RecordingLogger {
    inner: env_logger::Logger,
}

impl log::Log for RecordingLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        log::Log::enabled(&self.inner, metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if !log::Log::enabled(&self.inner, record.metadata()) {
            return;
        }
        log::Log::log(&self.inner, record);
        push_entry(record);
    }

    fn flush(&self) {
        log::Log::flush(&self.inner);
    }
}

fn push_entry(record: &log::Record<'_>) {
    let entry = Entry {
        level: record.level(),
        time: chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
        thread: std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned(),
        target: record.target().to_owned(),
        message: record.args().to_string(),
    };
    let mut recent = RECENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if recent.len() >= MAX_RECENT_ENTRIES {
        recent.pop_front();
    }
    recent.push_back(entry);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

fn format_record(buf: &mut env_logger::fmt::Formatter, record: &log::Record<'_>) -> io::Result<()> {
    let timestamp = chrono::Local::now().format(TIMESTAMP_FORMAT);
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("unnamed");
    writeln!(
        buf,
        "{timestamp} {:<5} [{name}] {}: {}",
        record.level(),
        record.target(),
        record.args()
    )
}

/// Captures panics before `panic = "abort"` ends the process.
///
/// The default hook still runs, so terminal users keep the usual message; the
/// file gains the location and, with `RUST_BACKTRACE=1`, a backtrace.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown location".to_owned());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        log::error!(target: "panic", "panic at {location}: {payload}");
        if std::env::var_os("RUST_BACKTRACE").is_some_and(|value| value != "0") {
            log::error!(target: "panic", "{}", std::backtrace::Backtrace::force_capture());
        }
        default_hook(info);
    }));
}

/// Writes every record to stderr and, while enabled, the rotating file.
///
/// stderr failures are ignored on purpose: a Windows GUI launch has no console,
/// so stderr writes fail there. `eprintln!` would panic in that situation and
/// abort the application; this sink must never do that.
struct TeeWriter {
    terminal: io::Stderr,
}

impl TeeWriter {
    fn new() -> Self {
        Self {
            terminal: io::stderr(),
        }
    }
}

impl io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = self.terminal.write_all(buf);
        // The sink reopens by itself, so a toggle takes effect without a
        // restart and this writer never tracks the switch.
        write_file(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = self.terminal.flush();
        flush_file();
        Ok(())
    }
}

/// An append-only log file that rotates itself by size.
///
/// Each record is flushed immediately: the workspace builds with
/// `panic = "abort"`, so destructors do not run and buffered tails would be
/// lost exactly when a crash report needs them most.
struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    limit: u64,
}

impl RotatingFile {
    fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_limit(path, MAX_LOG_BYTES)
    }

    fn open_with_limit(path: PathBuf, limit: u64) -> io::Result<Self> {
        let directory = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the log file has no parent directory",
            )
        })?;
        fs::create_dir_all(directory)?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let mut rotating = Self {
            path,
            file: Some(file),
            written,
            limit,
        };
        rotating.write_session_header()?;
        Ok(rotating)
    }

    /// A plain first line naming the build and host, so a pasted log is
    /// self-describing even when the recorder was never reached.
    fn write_session_header(&mut self) -> io::Result<()> {
        let header = format!("{} log opened at {}", build_stamp(), opened_at());
        if let Some(file) = &mut self.file {
            file.write_all(header.as_bytes())?;
            file.write_all(b"\n")?;
            file.flush()?;
        }
        self.written = self.written.saturating_add(header.len() as u64 + 1);
        Ok(())
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.written.saturating_add(buf.len() as u64) > self.limit {
            self.rotate()?;
        }
        if self.file.is_none() {
            self.reopen()?;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("the log file is closed and could not be reopened"))?;
        file.write_all(buf)?;
        file.flush()?;
        self.written = self.written.saturating_add(buf.len() as u64);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }

    /// Closes the current file first: Windows cannot rename an open file.
    ///
    /// A rotated file that a shorter history never created is not an error,
    /// so a missing source is ignored; anything else is reported.
    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        remove_if_present(&rotated_path(&self.path, KEPT_LOG_FILES))?;
        for index in (1..KEPT_LOG_FILES).rev() {
            rename_if_present(
                &rotated_path(&self.path, index),
                &rotated_path(&self.path, index + 1),
            )?;
        }
        rename_if_present(&self.path, &rotated_path(&self.path, 1))?;
        self.written = 0;
        self.reopen()
    }

    fn reopen(&mut self) -> io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.file = Some(file);
        self.written = 0;
        // The new file starts with the stamp, like a freshly opened one: a
        // rotation must not leave a file whose build nobody can name.
        self.write_session_header()
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn rename_if_present(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn rotated_path(path: &Path, index: u32) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{name}.{index}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    /// The ring and the file switches are process-wide, so tests that touch
    /// them take turns.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset() -> MutexGuard<'static, ()> {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_recent();
        guard
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("freya-nle-logging-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("scratch directory");
        directory
    }

    fn rotated_entries(directory: &Path, name: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .expect("scratch directory")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|entry| entry.starts_with(name))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_ring_keeps_a_bounded_number_of_recent_records() {
        let _guard = reset();
        for index in 0..MAX_RECENT_ENTRIES + 25 {
            push_entry(
                &log::Record::builder()
                    .args(format_args!("record {index}"))
                    .level(Level::Info)
                    .target("test")
                    .build(),
            );
        }
        let (entries, _) = snapshot();
        assert_eq!(entries.len(), MAX_RECENT_ENTRIES);
        assert!(entries.first().unwrap().line().ends_with("record 25"));
        assert!(entries.last().unwrap().line().ends_with("record 524"));
    }

    #[test]
    fn clearing_forgets_records_and_bumps_the_generation() {
        let _guard = reset();
        let before = generation();
        push_entry(
            &log::Record::builder()
                .args(format_args!("hello"))
                .level(Level::Warn)
                .target("test")
                .build(),
        );
        assert!(generation() > before);
        clear_recent();
        assert!(snapshot().0.is_empty());
    }

    #[test]
    fn a_failed_open_is_reported_and_retried_after_a_toggle() {
        let directory = scratch_dir("retry");
        // A regular file where the log directory should be makes the open fail.
        let blocked = directory.join("blocked");
        fs::write(&blocked, b"").expect("blocking file");
        let logs = blocked.join("logs");
        let mut sink = FileSink {
            file: None,
            epoch: 1,
            gave_up: false,
            error: None,
        };
        reconcile(&mut sink, 1, true, &logs);
        assert!(sink.gave_up);
        assert!(sink.error.is_some(), "the failure must be reported");

        // The directory becomes usable, and the switch is toggled again: a new
        // epoch is a fresh attempt, so the open is retried.
        fs::remove_file(&blocked).expect("remove blocking file");
        reconcile(&mut sink, 2, false, &logs);
        reconcile(&mut sink, 2, true, &logs);
        assert!(!sink.gave_up);
        assert!(sink.error.is_none(), "a successful retry clears the report");
        assert!(logs.join(LOG_FILE_NAME).exists());

        // Turning the switch off closes the file and stops reporting a
        // failure: nothing is expected to be written while it is off.
        reconcile(&mut sink, 2, false, &logs);
        assert!(sink.file.is_none(), "disabling closes the file");
        assert!(sink.error.is_none());
    }

    #[test]
    fn a_transcript_stamps_the_build_and_keeps_each_line() {
        let _guard = reset();
        push_entry(
            &log::Record::builder()
                .args(format_args!("first"))
                .level(Level::Warn)
                .target("preview")
                .build(),
        );
        let text = transcript();
        assert!(text.starts_with("Freya NLE "), "{text}");
        assert!(text.contains("WARN"), "{text}");
        assert!(text.contains("preview: first"), "{text}");
    }

    #[test]
    fn a_session_stamps_the_file_with_the_build() {
        let directory = scratch_dir("header");
        let path = directory.join("test.log");
        let _ = RotatingFile::open_with_limit(path.clone(), 4096).expect("open");
        let contents = fs::read_to_string(&path).expect("read");
        assert!(contents.starts_with("Freya NLE "), "{contents}");
        assert!(contents.contains(std::env::consts::OS), "{contents}");
    }

    #[test]
    fn a_long_session_rotates_and_keeps_a_bounded_history() {
        let directory = scratch_dir("rotate");
        let path = directory.join("test.log");
        let mut file = RotatingFile::open_with_limit(path.clone(), 128).expect("open");
        for _ in 0..8 {
            file.write(b"0123456789abcdef0123456789abcdef\n")
                .expect("write");
        }
        let entries = rotated_entries(&directory, "test.log");
        // The current file plus exactly KEPT_LOG_FILES rotated ones, never more.
        assert_eq!(entries.len() as u32, KEPT_LOG_FILES + 1, "{entries:?}");
        assert!(path.exists());
        // Each file names the build that wrote it, including the ones a
        // rotation created.
        let current = fs::read_to_string(&path).expect("read");
        assert!(current.starts_with("Freya NLE "), "{current}");
        let first = fs::read_to_string(directory.join("test.log.1")).expect("read rotated");
        assert!(first.starts_with("Freya NLE "), "{first}");
    }

    #[test]
    fn rotation_names_replace_the_extension() {
        assert_eq!(
            rotated_path(Path::new("/tmp/freya-nle.log"), 2),
            PathBuf::from("/tmp/freya-nle.log.2")
        );
    }
}

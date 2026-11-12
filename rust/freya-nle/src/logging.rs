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
    let target = env_logger::Target::Pipe(Box::new(TeeWriter::new(log_directory())));
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
        FILE_ENABLED.store(true, Ordering::Relaxed);
        announce_file();
    } else {
        // Recorded before the switch flips, so this line is still in the file
        // it closes and the file does not end mid-session without a reason.
        log::info!(target: "app", "log file disabled");
        FILE_ENABLED.store(false, Ordering::Relaxed);
    }
}

/// Names the file the current session is appending to, once it is open.
fn announce_file() {
    log::info!(target: "app", "log file: {}", file_path().display());
}

pub(super) fn file_enabled() -> bool {
    FILE_ENABLED.load(Ordering::Relaxed)
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

/// A snapshot of the recent records, oldest first.
pub(super) fn recent() -> Vec<Entry> {
    let recent = RECENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    recent.iter().cloned().collect()
}

pub(super) fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Forgets the in-memory records behind the log window; the file, if open,
/// keeps its history.
pub(super) fn clear_recent() {
    RECENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
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
    for entry in recent() {
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
    directory: PathBuf,
    file: Option<RotatingFile>,
    /// Set when the file could not be opened, so an unwritable data directory
    /// is not retried on every record. Toggling the switch clears it.
    gave_up: bool,
}

impl TeeWriter {
    fn new(directory: PathBuf) -> Self {
        Self {
            terminal: io::stderr(),
            directory,
            file: None,
            gave_up: false,
        }
    }

    /// Opens or closes the file to match the current switch. Runs on every
    /// write so a Preferences toggle takes effect without a restart.
    fn sync(&mut self) {
        if FILE_ENABLED.load(Ordering::Relaxed) {
            if self.file.is_none() && !self.gave_up {
                self.file = RotatingFile::open(self.directory.join(LOG_FILE_NAME));
                self.gave_up = self.file.is_none();
            }
        } else if self.file.is_some() || self.gave_up {
            self.file = None;
            self.gave_up = false;
        }
    }
}

impl io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.sync();
        let _ = self.terminal.write_all(buf);
        if let Some(file) = &mut self.file {
            file.write(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = self.terminal.flush();
        if let Some(file) = &mut self.file {
            file.flush();
        }
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
    fn open(path: PathBuf) -> Option<Self> {
        Self::open_with_limit(path, MAX_LOG_BYTES)
    }

    fn open_with_limit(path: PathBuf, limit: u64) -> Option<Self> {
        let directory = path.parent()?;
        fs::create_dir_all(directory).ok()?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let mut rotating = Self {
            path,
            file: Some(file),
            written,
            limit,
        };
        rotating.write_session_header();
        Some(rotating)
    }

    /// A plain first line naming the build and host, so a pasted log is
    /// self-describing even when the recorder was never reached.
    fn write_session_header(&mut self) {
        let header = format!("{} log opened at {}", build_stamp(), opened_at());
        if let Some(file) = &mut self.file {
            let _ = file.write_all(header.as_bytes());
            let _ = file.write_all(b"\n");
            let _ = file.flush();
        }
        self.written = self.written.saturating_add(header.len() as u64 + 1);
    }

    fn write(&mut self, buf: &[u8]) {
        if self.written.saturating_add(buf.len() as u64) > self.limit {
            self.rotate();
        }
        if self.file.is_none() {
            self.reopen();
        }
        if let Some(file) = &mut self.file {
            if file.write_all(buf).is_ok() {
                self.written = self.written.saturating_add(buf.len() as u64);
            }
            let _ = file.flush();
        }
    }

    fn flush(&mut self) {
        if let Some(file) = &mut self.file {
            let _ = file.flush();
        }
    }

    /// Closes the current file first: Windows cannot rename an open file.
    fn rotate(&mut self) {
        self.file = None;
        let _ = fs::remove_file(rotated_path(&self.path, KEPT_LOG_FILES));
        for index in (1..KEPT_LOG_FILES).rev() {
            let _ = fs::rename(
                rotated_path(&self.path, index),
                rotated_path(&self.path, index + 1),
            );
        }
        let _ = fs::rename(&self.path, rotated_path(&self.path, 1));
        self.written = 0;
        self.reopen();
    }

    fn reopen(&mut self) {
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            self.file = Some(file);
            self.written = 0;
        }
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
        let entries = recent();
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
        assert!(recent().is_empty());
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
            file.write(b"0123456789abcdef0123456789abcdef\n");
        }
        let entries = rotated_entries(&directory, "test.log");
        // The current file plus exactly KEPT_LOG_FILES rotated ones, never more.
        assert_eq!(entries.len() as u32, KEPT_LOG_FILES + 1, "{entries:?}");
        assert!(path.exists());
    }

    #[test]
    fn rotation_names_replace_the_extension() {
        assert_eq!(
            rotated_path(Path::new("/tmp/freya-nle.log"), 2),
            PathBuf::from("/tmp/freya-nle.log.2")
        );
    }
}

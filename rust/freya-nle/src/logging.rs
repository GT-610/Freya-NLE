//! Process-wide logging: stderr plus a rotating file, `RUST_LOG` filtering,
//! and a panic hook that leaves a record on disk for support.
//!
//! The binary installs this once, before anything else runs. Library crates
//! only use the `log` facade and never install a logger, so tests stay quiet
//! and the sinks remain a single decision made here.
//!
//! stdout is reserved for machine-readable protocols (`--validate-gpu`'s JSON,
//! `--help`, `--version`). Everything diagnostic goes to stderr and the file.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// Directory under the application data root that holds session logs.
const LOG_DIRECTORY: &str = "logs";
const LOG_FILE_NAME: &str = "freya-nle.log";
/// `freya-nle.log` is rotated to `.1` when it would exceed this size.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// How many rotated files are kept beside the current one.
const KEPT_LOG_FILES: u32 = 3;

/// `RUST_LOG` wins when set. The default keeps the application at `info` and
/// silences third-party stacks that log per-frame or per-widget at `debug`.
const DEFAULT_FILTER: &str =
    "info,wgpu=warn,naga=warn,winit=warn,calloop=warn,softbuffer=warn,zbus=warn";

/// Installs the global logger, unless one is already installed.
///
/// Returns whether this call installed it. The log file falls back to a
/// stderr-only sink when the data directory cannot be created or written.
pub(super) fn init() -> bool {
    let filter = std::env::var("RUST_LOG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_FILTER.to_owned());
    let path = log_file_path();
    let target = env_logger::Target::Pipe(Box::new(TeeWriter::new(path.clone())));
    let mut builder = env_logger::Builder::new();
    builder
        .parse_filters(&filter)
        .target(target)
        .format(format_record);
    if builder.try_init().is_err() {
        return false;
    }
    install_panic_hook();
    if let Some(path) = path.filter(|path| path.exists()) {
        log::info!(target: "app", "logging to {}", path.display());
    }
    true
}

/// The log file location: `FREYA_NLE_LOG_DIR` when set, otherwise
/// `<application data root>/logs/freya-nle.log`.
fn log_file_path() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("FREYA_NLE_LOG_DIR") {
        return Some(Path::new(&directory).join(LOG_FILE_NAME));
    }
    Some(
        freya_nle_app::settings::application_data_root()
            .join(LOG_DIRECTORY)
            .join(LOG_FILE_NAME),
    )
}

fn format_record(buf: &mut env_logger::fmt::Formatter, record: &log::Record<'_>) -> io::Result<()> {
    let timestamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z");
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

/// Writes every record to stderr and, when available, the rotating file.
///
/// stderr failures are ignored on purpose: a Windows GUI launch has no console,
/// so stderr writes fail there. `eprintln!` would panic in that situation and
/// abort the application; this sink must never do that.
struct TeeWriter {
    terminal: io::Stderr,
    file: Option<RotatingFile>,
}

impl TeeWriter {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            terminal: io::stderr(),
            file: path.and_then(RotatingFile::open),
        }
    }
}

impl io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
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
        let header = format!(
            "Freya NLE {} starting on {} ({}) at {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
        );
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

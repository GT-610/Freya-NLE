//! Routes FFmpeg's `av_log` output into the `log` facade.
//!
//! FFmpeg prints diagnostics to stderr through its own C logging by default,
//! outside the application's filtering, rotation, and panic handling.
//! Installing a callback moves those messages into the same records, so a
//! decoder warning ends up beside the preview warning that explains it.
//!
//! The safe wrapper does not expose `av_log_set_callback`, so this module is
//! the crate's single `unsafe` boundary. Every call into the C library is
//! documented with its safety argument; the callback itself is written to run
//! on any thread (FFmpeg calls it from codec worker threads) and never panics.

#![allow(unsafe_code)]

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

use ffmpeg::sys;

/// Enough for the longest FFmpeg diagnostic; longer lines are truncated.
const LINE_BYTES: usize = 1024;

/// The type FFmpeg's C prototypes really take for `va_list`.
///
/// Where `va_list` is an array (the SysV x86-64 ABI), a C parameter of that
/// type decays to a pointer: bindgen keeps the alias as the array while it
/// declares the functions with the pointer. Naming either form by hand would
/// tie this module to one target, so the binding's own alias decides.
trait DecayedVaList {
    type Arg;
}

impl<T> DecayedVaList for [T; 1] {
    /// An array parameter decays to a pointer to its first element.
    type Arg = *mut T;
}

impl<T> DecayedVaList for *mut T {
    /// A pointer parameter stays exactly as it is.
    type Arg = *mut T;
}

/// What the callback receives and `av_log_format_line2` consumes.
type VaArgs = <sys::va_list as DecayedVaList>::Arg;

thread_local! {
    /// FFmpeg's format helper expects a per-thread prefix flag that persists
    /// across calls, so a continuation line is not stamped twice.
    static PRINT_PREFIX: Cell<c_int> = const { Cell::new(1) };
}

/// Installs the callback. Safe to call more than once; the last call wins.
pub fn install() {
    // SAFETY: `av_log_set_callback` accepts a function with this exact
    // signature and is documented as thread-safe to install. The callback
    // stays alive for the process lifetime.
    unsafe {
        sys::av_log_set_callback(Some(callback));
    }
}

/// The C callback. Must not unwind: FFmpeg frames are not `extern "C"`
/// unwind-safe, and `panic = "abort"` makes an escape fatal anywhere.
unsafe extern "C" fn callback(avcl: *mut c_void, level: c_int, fmt: *const c_char, va: VaArgs) {
    let _ = catch_unwind(AssertUnwindSafe(|| log_line(avcl, level, fmt, va)));
}

fn log_line(avcl: *mut c_void, level: c_int, fmt: *const c_char, va: VaArgs) {
    if fmt.is_null() {
        return;
    }
    let mut line = [0 as c_char; LINE_BYTES];
    let written = PRINT_PREFIX
        .try_with(|slot| {
            let mut print_prefix = slot.get();
            // SAFETY: `fmt` was checked non-null, `line` is a writable buffer
            // of `LINE_BYTES`, and `va` comes from FFmpeg for this call. The
            // helper only formats the message; it stores no pointer.
            let written = unsafe {
                sys::av_log_format_line2(
                    avcl,
                    level,
                    fmt,
                    va,
                    line.as_mut_ptr(),
                    LINE_BYTES as c_int,
                    &mut print_prefix,
                )
            };
            slot.set(print_prefix);
            written
        })
        .unwrap_or(0);
    if written <= 0 {
        return;
    }
    // SAFETY: `av_log_format_line2` null-terminates the line it writes.
    let message = unsafe { CStr::from_ptr(line.as_ptr()) }.to_string_lossy();
    let message = message.trim_end_matches(['\r', '\n']);
    if message.is_empty() {
        return;
    }
    log::log!(target: "ffmpeg", map_level(level), "{message}");
}

fn map_level(level: c_int) -> log::Level {
    if level <= sys::AV_LOG_ERROR {
        log::Level::Error
    } else if level <= sys::AV_LOG_WARNING {
        log::Level::Warn
    } else if level <= sys::AV_LOG_INFO {
        log::Level::Info
    } else if level <= sys::AV_LOG_DEBUG {
        log::Level::Debug
    } else {
        log::Level::Trace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Collects what the bridge hands to the `log` facade.
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl log::Log for Capture {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            if let Ok(mut messages) = self.0.lock() {
                messages.push(format!("{} {}", record.target(), record.args()));
            }
        }

        fn flush(&self) {}
    }

    #[test]
    fn the_va_argument_is_what_the_c_prototype_takes() {
        // An array `va_list` decays to a pointer; one that is already a pointer
        // is passed through. Both cases must name a single pointer type.
        let _: <[u64; 1] as DecayedVaList>::Arg = std::ptr::null_mut();
        let _: <*mut u64 as DecayedVaList>::Arg = std::ptr::null_mut();
    }

    #[test]
    fn ffmpeg_diagnostics_reach_the_log_facade() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let logger: &'static Capture = Box::leak(Box::new(Capture(captured.clone())));
        if log::set_logger(logger).is_err() {
            // Another test owns the process-wide logger; the bridge is still
            // installed, so there is nothing left to prove here.
            return;
        }
        log::set_max_level(log::LevelFilter::Trace);
        install();
        let path =
            std::env::temp_dir().join(format!("freya-nle-ffmpeg-log-{}.bin", std::process::id()));
        std::fs::write(&path, b"not a media file").expect("write junk file");
        let _ = ffmpeg::format::input(&path);
        let _ = std::fs::remove_file(&path);
        let messages = captured.lock().expect("captured messages");
        assert!(
            messages
                .iter()
                .any(|message| message.starts_with("ffmpeg ") && !message.trim().eq("ffmpeg")),
            "expected a captured FFmpeg diagnostic, got {messages:?}"
        );
    }
}

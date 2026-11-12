//! The log window: the recent records plus the actions AviUtl2's log window
//! has (clear, copy through text selection, save, and reveal the file folder).
//!
//! The window only presents what `logging` already collected: a read-only
//! `TextEdit` with the newest line on top. The timer exists so records from
//! other threads (packages, decoding, plugins) appear without polling the UI
//! on every write.

use crate::dialogs::{show_and_redraw, show_info_dialog};
use crate::localization::localized;
use crate::{LogWindow, logging};
use freya_nle_app::settings::SettingsStore;
use log::Level;
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

/// How fresh the view is: short enough to feel live, long enough that an idle
/// window costs one atomic read.
const REFRESH_INTERVAL: Duration = Duration::from_millis(300);
/// The window is plain text; a long session should not paint thousands of
/// lines every time one more arrives.
const MAX_DISPLAY_LINES: usize = 400;

type Settings = Rc<RefCell<SettingsStore>>;

thread_local! {
    /// A dropped `Timer` stops firing, so the handle has to outlive `install`.
    static REFRESH_TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
    /// The filter choice and the generation already painted. One log window
    /// exists per process, so module-level state is the simplest owner.
    static VIEW: RefCell<ViewState> = const { RefCell::new(ViewState { filter: 0, generation: u64::MAX }) };
}

#[derive(Clone, Copy)]
struct ViewState {
    filter: i32,
    generation: u64,
}

pub(super) fn install(window: &LogWindow, settings: Settings) {
    refresh(window);
    let filter_window = window.as_weak();
    window.on_filter_changed(move |index| {
        VIEW.with(|view| view.borrow_mut().filter = index);
        if let Some(window) = filter_window.upgrade() {
            draw(&window);
        }
    });
    let clear_window = window.as_weak();
    window.on_clear_log(move || {
        logging::clear_recent();
        if let Some(window) = clear_window.upgrade() {
            draw(&window);
        }
    });
    let copy_window = window.as_weak();
    window.on_copy_log(move || {
        if let Some(window) = copy_window.upgrade() {
            copy(&window);
        }
    });
    window.on_save_log(save);
    window.on_open_log_folder(open_folder);
    let toggle_settings = settings;
    let toggle_window = window.as_weak();
    window.on_file_enabled_changed(move |enabled| {
        logging::set_file_enabled(enabled);
        persist_file_enabled(&toggle_settings, enabled);
        if let Some(window) = toggle_window.upgrade() {
            sync_file_state(&window);
        }
    });
    let timer_window = window.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, REFRESH_INTERVAL, move || {
        if let Some(window) = timer_window.upgrade() {
            refresh(&window);
        }
    });
    REFRESH_TIMER.with(|slot| *slot.borrow_mut() = Some(timer));
}

/// Re-renders when a record or a clear arrived since the last paint.
pub(super) fn refresh(window: &LogWindow) {
    let generation = logging::generation();
    let painted = VIEW.with(|view| view.borrow().generation);
    if painted != generation {
        draw(window);
    }
}

/// Shows the window, taking a fresh state from the logger.
pub(super) fn show(window: &LogWindow) {
    VIEW.with(|view| view.borrow_mut().generation = u64::MAX);
    refresh(window);
    let _ = show_and_redraw(window);
}

fn draw(window: &LogWindow) {
    let filter = VIEW.with(|view| view.borrow().filter);
    let entries = logging::recent();
    let mut text = String::new();
    for entry in entries
        .iter()
        .rev()
        .filter(|entry| passes(entry.level(), filter))
        .take(MAX_DISPLAY_LINES)
    {
        text.push_str(&entry.line());
        text.push('\n');
    }
    window.set_log_text(text.into());
    sync_file_state(window);
    VIEW.with(|view| view.borrow_mut().generation = logging::generation());
}

fn sync_file_state(window: &LogWindow) {
    if window.get_file_enabled() != logging::file_enabled() {
        window.set_file_enabled(logging::file_enabled());
    }
    window.set_file_path(
        logging::file_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into(),
    );
}

fn passes(level: Level, filter: i32) -> bool {
    // `Level` orders from most to least severe: Error < Warn < Info < Debug <
    // Trace, so "and above" means the level is at or below the threshold.
    match filter {
        1 => level <= Level::Info,
        2 => level <= Level::Warn,
        3 => level <= Level::Error,
        _ => true,
    }
}

fn persist_file_enabled(settings: &Settings, enabled: bool) {
    let mut replacement = serde_json::Map::new();
    replacement.insert("logFile".to_owned(), serde_json::Value::Bool(enabled));
    if let Err(error) = settings.borrow_mut().apply(replacement) {
        log::warn!(target: "settings", "Could not save the log setting: {error}");
    }
}

fn copy(window: &LogWindow) {
    if let Err(error) = copy_to_clipboard(&logging::transcript()) {
        log::warn!(target: "app", "could not copy the log: {error}");
        show_info_dialog(
            localized(
                "Could not copy the log",
                "无法复制日志",
                "ログをコピーできません",
            ),
            localized(
                "Select the log text and use the copy shortcut instead.",
                "请改为选中日志文本后使用复制快捷键。",
                "ログのテキストを選択してコピーショートカットを使用してください。",
            ),
        );
        let _ = window;
    }
}

/// Slint exposes no clipboard API, so the platform's usual helper is used:
/// `pbcopy` on macOS, `clip` on Windows, and Wayland/X11 tools on Linux.
fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        spawn_with_input("pbcopy", &[], text)
    }
    #[cfg(target_os = "windows")]
    {
        spawn_with_input("cmd", &["/c", "clip"], text)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let candidates: [(&str, &[&str]); 3] = [
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        let mut last_error =
            std::io::Error::new(std::io::ErrorKind::NotFound, "no clipboard command found");
        for (program, arguments) in candidates {
            match spawn_with_input(program, arguments, text) {
                Ok(()) => return Ok(()),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }
}

fn spawn_with_input(program: &str, arguments: &[&str], text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut child = std::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{program} exited with {status}"
        )))
    }
}

fn save() {
    let title = localized("Save log", "保存日志", "ログを保存");
    let default_name = logging::file_path()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "freya-nle.log".to_owned());
    let Some(path) = rfd::FileDialog::new()
        .set_title(title)
        .set_file_name(default_name)
        .add_filter("Log files", &["log", "txt"])
        .save_file()
    else {
        return;
    };
    match std::fs::write(&path, logging::transcript()) {
        Ok(()) => {
            log::info!(target: "app", "log saved to {}", path.display());
        }
        Err(error) => {
            log::warn!(target: "app", "could not save the log: {error}");
            show_info_dialog(
                localized(
                    "Could not save the log",
                    "无法保存日志",
                    "ログを保存できません",
                ),
                &format!("{}\n\n{error}", path.display()),
            );
        }
    }
}

fn open_folder() {
    let Some(directory) = logging::log_directory() else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(&directory) {
        log::warn!(target: "app", "could not create the log folder: {error}");
        return;
    }
    match reveal_directory(&directory) {
        Ok(()) => {}
        Err(error) => {
            log::warn!(target: "app", "could not open the log folder: {error}");
            show_info_dialog(
                localized(
                    "Could not open the log folder",
                    "无法打开日志文件夹",
                    "ログフォルダを開けません",
                ),
                &directory.display().to_string(),
            );
        }
    }
}

fn reveal_directory(directory: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command.arg(directory).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_names_match_the_level_order() {
        assert!(passes(Level::Error, 3));
        assert!(passes(Level::Warn, 2));
        assert!(!passes(Level::Info, 2));
        assert!(passes(Level::Info, 1));
        assert!(!passes(Level::Debug, 1));
        assert!(passes(Level::Trace, 0));
    }
}

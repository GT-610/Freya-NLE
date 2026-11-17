// Interaction regressions for the recovery and backup-history dialogs.
//
// Both windows list many similar rows and repeated button text, so each row's
// buttons carry an accessible label naming the snapshot or version they act on.
// Both also ignored Escape, which every other dialog in the application honours,
// so the key handling is driven here through real window events.
//
// The testing backend exposes no accessibility tree, so the labels themselves are
// not asserted; these tests pin the behaviour and the per-row identity the labels
// interpolate.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, VecModel, platform::WindowEvent};

slint::include_modules!();

fn press_escape(window: &impl ComponentHandle) {
    window.window().dispatch_event(WindowEvent::KeyPressed {
        text: "\u{1b}".into(),
    });
    window.window().dispatch_event(WindowEvent::KeyReleased {
        text: "\u{1b}".into(),
    });
}

// The focus scope takes focus on the next event-loop turn.
fn settle_focus() {
    i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2));
}

fn recovery_entry(id: &str, name: &str, valid: bool) -> RecoveryEntryData {
    RecoveryEntryData {
        id: id.into(),
        name: name.into(),
        saved_at: "2026-11-17 12:00".into(),
        original_project_url: format!("/projects/{name}.aviqtl").into(),
        valid,
        error: if valid {
            "".into()
        } else {
            "unreadable".into()
        },
    }
}

#[test]
fn recovery_window_closes_on_escape() {
    i_slint_backend_testing::init_no_event_loop();
    let window = ProjectRecoveryWindow::new().unwrap();
    let closes = Rc::new(Cell::new(0));
    let count = closes.clone();
    window.on_close_window(move || count.set(count.get() + 1));
    window.show().unwrap();
    settle_focus();

    press_escape(&window);
    assert_eq!(closes.get(), 1);
}

#[test]
fn escape_dismisses_the_discard_confirmation_before_the_window() {
    i_slint_backend_testing::init_no_event_loop();
    let window = ProjectRecoveryWindow::new().unwrap();
    let closes = Rc::new(Cell::new(0));
    let count = closes.clone();
    window.on_close_window(move || count.set(count.get() + 1));
    window.show().unwrap();
    settle_focus();

    // Escape must not discard anything: it only closes the confirmation, so the
    // snapshot is still there and the window still open.
    window.set_discard_confirmation_visible(true);
    press_escape(&window);
    assert!(!window.get_discard_confirmation_visible());
    assert_eq!(closes.get(), 0);

    press_escape(&window);
    assert_eq!(closes.get(), 1);
}

#[test]
fn recovery_rows_keep_the_identity_their_buttons_announce() {
    i_slint_backend_testing::init_no_event_loop();
    let window = ProjectRecoveryWindow::new().unwrap();
    let recovered = Rc::new(RefCell::new(Vec::<String>::new()));
    let log = recovered.clone();
    window.on_recover_project(move |id| log.borrow_mut().push(id.to_string()));
    let discarded = Rc::new(RefCell::new(Vec::<String>::new()));
    let log = discarded.clone();
    window.on_discard_recovery(move |id| log.borrow_mut().push(id.to_string()));
    window.show().unwrap();

    let rows = vec![
        recovery_entry("first", "Holiday", true),
        recovery_entry("second", "Wedding", false),
    ];
    window.set_recoveries(ModelRc::new(VecModel::from(rows)));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2));

    // Each row carries a distinct name and ID, which is what the per-row
    // accessible labels interpolate; two "Discard" buttons acting on different
    // snapshots are only safe because the identity travels with the row.
    assert_eq!(window.get_recoveries().row_count(), 2);
    assert_eq!(window.get_recoveries().row_data(0).unwrap().id, "first");
    assert_eq!(window.get_recoveries().row_data(1).unwrap().name, "Wedding");

    window.invoke_recover_project("first".into());
    assert_eq!(&*recovered.borrow(), &["first".to_string()]);
}

fn backup_project(label: &str, times: &[&str]) -> BackupProjectData {
    BackupProjectData {
        key: format!("/backups/{label}").into(),
        label: label.into(),
        detail: "/backups".into(),
        entries: ModelRc::new(VecModel::from(
            times
                .iter()
                .map(|time| BackupEntryData {
                    file_name: format!("{label}-{time}.aviqtl").into(),
                    saved_at: (*time).into(),
                    size_text: "12 KiB".into(),
                })
                .collect::<Vec<_>>(),
        )),
    }
}

#[test]
fn backup_history_closes_on_escape() {
    i_slint_backend_testing::init_no_event_loop();
    let window = BackupHistoryWindow::new().unwrap();
    let closes = Rc::new(Cell::new(0));
    let count = closes.clone();
    window.on_close_window(move || count.set(count.get() + 1));
    window.show().unwrap();
    settle_focus();

    press_escape(&window);
    assert_eq!(closes.get(), 1);
}

#[test]
fn backup_history_keeps_each_version_addressable() {
    i_slint_backend_testing::init_no_event_loop();
    let window = BackupHistoryWindow::new().unwrap();
    let opened = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let log = opened.clone();
    window
        .on_open_entry(move |key, file| log.borrow_mut().push((key.to_string(), file.to_string())));
    window.show().unwrap();

    window.set_projects(ModelRc::new(VecModel::from(vec![backup_project(
        "Holiday",
        &["12:00", "12:30"],
    )])));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2));

    let projects = window.get_projects();
    assert_eq!(projects.row_count(), 1);
    let entries = projects.row_data(0).unwrap().entries;
    assert_eq!(entries.row_count(), 2);
    // Two rows carry distinct file names, so the per-row labels differ.
    assert_ne!(
        entries.row_data(0).unwrap().file_name,
        entries.row_data(1).unwrap().file_name
    );

    window.invoke_open_entry("/backups/Holiday".into(), "Holiday-12:30.aviqtl".into());
    assert_eq!(
        &*opened.borrow(),
        &[(
            "/backups/Holiday".to_string(),
            "Holiday-12:30.aviqtl".to_string()
        )]
    );
}

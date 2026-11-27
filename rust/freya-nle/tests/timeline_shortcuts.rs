use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, VecModel};

slint::include_modules!();

fn press_key(window: &TimelineWindow) {
    window
        .window()
        .dispatch_event(WindowEvent::KeyPressed { text: "a".into() });
    window
        .window()
        .dispatch_event(WindowEvent::KeyReleased { text: "a".into() });
}

#[test]
fn timeline_shortcuts_pause_while_an_overlay_is_open() {
    i_slint_backend_testing::init_no_event_loop();
    let window = TimelineWindow::new().unwrap();
    let calls = Rc::new(Cell::new(0));
    let count = calls.clone();
    window.on_keyboard_shortcut(move |_, _, _, _, _| {
        count.set(count.get() + 1);
        true
    });
    window.show().unwrap();

    // Closing a popup restores focus to the timeline on the next timer tick.
    window.set_object_picker_visible(true);
    window.set_object_picker_visible(false);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2));
    press_key(&window);
    assert_eq!(calls.get(), 1);

    // Before the popup takes focus, its visibility must already block shortcuts.
    window.set_object_picker_visible(true);
    press_key(&window);
    assert_eq!(calls.get(), 1);
    window.set_object_picker_visible(false);
    i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2));
    press_key(&window);
    assert_eq!(calls.get(), 2);

    window.set_save_confirmation_visible(true);
    press_key(&window);
    assert_eq!(calls.get(), 2);
    window.set_save_confirmation_visible(false);
    press_key(&window);
    assert_eq!(calls.get(), 3);
}

#[test]
fn returning_to_timeline_commits_the_focused_caption_before_handing_off() {
    i_slint_backend_testing::init_no_event_loop();
    let settings = ObjectSettingsWindow::new().unwrap();
    settings.window().set_size(PhysicalSize::new(680, 480));
    settings.set_has_selection(true);
    settings.set_setting_rows(ModelRc::new(VecModel::from(vec![ObjectSettingRowData {
        row_kind: "text".into(),
        param_name: "text".into(),
        label: "Caption".into(),
        interactive: true,
        ..Default::default()
    }])));
    let timeline = TimelineWindow::new().unwrap();
    timeline.show().unwrap();
    settings.show().unwrap();

    let events = Rc::new(RefCell::new(Vec::new()));
    let record = events.clone();
    settings.on_preview_parameter_text(move |_, _, _, _, text| {
        assert_eq!(text, "A");
        record.borrow_mut().push("preview");
    });
    let record = events.clone();
    settings.on_finish_parameter_edit(move || record.borrow_mut().push("finish"));
    let record = events.clone();
    let timeline_weak = timeline.as_weak();
    settings.on_return_to_timeline(move || {
        record.borrow_mut().push("return");
        timeline_weak
            .upgrade()
            .unwrap()
            .invoke_focus_editing_surface();
    });
    let record = events.clone();
    timeline.on_keyboard_shortcut(move |_, _, _, _, _| {
        record.borrow_mut().push("timeline");
        true
    });

    let position = LogicalPosition::new(260.0, 75.0);
    settings
        .window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    settings
        .window()
        .dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
    settings
        .window()
        .dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    settings
        .window()
        .dispatch_event(WindowEvent::KeyPressed { text: "A".into() });
    settings.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::F6.into(),
    });
    i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2));
    press_key(&timeline);
    assert_eq!(
        *events.borrow(),
        ["preview", "finish", "return", "timeline"]
    );
}

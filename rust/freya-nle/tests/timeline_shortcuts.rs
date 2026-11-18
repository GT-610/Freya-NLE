use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, platform::WindowEvent};

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

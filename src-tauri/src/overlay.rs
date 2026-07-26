//! Non-interactive status capsule for dictation sessions.
//!
//! The window is never a control surface: it stays hidden while idle and
//! ignores pointer input whenever it is visible, preserving the user's target
//! editor as the foreground window.

use tauri::WebviewWindow;

use crate::session::SessionStatus;

pub fn prepare(window: &WebviewWindow) {
    let _ = window.set_ignore_cursor_events(true);
}

pub fn sync(window: &WebviewWindow, status: &SessionStatus) {
    let result = if status.phase.presents_overlay() {
        window.show()
    } else {
        window.hide()
    };
    let _ = result;
}

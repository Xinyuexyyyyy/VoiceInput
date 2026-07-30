//! Non-interactive status capsule for dictation sessions.
//!
//! The window is never a control surface: it stays hidden while idle and
//! ignores pointer input whenever it is visible, preserving the user's target
//! editor as the foreground window.

use tauri::WebviewWindow;

use crate::session::SessionStatus;

pub fn prepare(window: &WebviewWindow) {
    let _ = window.set_ignore_cursor_events(true);
    let _ = window.hide();
}

pub fn sync(window: &WebviewWindow, status: &SessionStatus) {
    let result = if status.phase.presents_overlay() {
        show_without_activating(window)
    } else {
        window.hide()
    };
    let _ = result;
}

#[cfg(windows)]
fn show_without_activating(window: &WebviewWindow) -> tauri::Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};

    let hwnd = HWND(window.hwnd()?.0);
    // The status window must never become the foreground input target.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    Ok(())
}

#[cfg(not(windows))]
fn show_without_activating(window: &WebviewWindow) -> tauri::Result<()> {
    window.show()
}

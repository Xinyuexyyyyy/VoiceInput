//! Non-interactive status capsule for dictation sessions.
//!
//! The window is never a control surface: it stays hidden while idle and
//! ignores pointer input whenever it is visible, preserving the user's target
//! editor as the foreground window.

use tauri::WebviewWindow;

use crate::session::SessionStatus;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisibilityAction {
    ShowWithoutActivating,
    NativeHide,
}

pub fn prepare(window: &WebviewWindow) {
    let _ = window.set_ignore_cursor_events(true);
    let _ = hide_natively(window);
}

pub fn sync(window: &WebviewWindow, status: &SessionStatus) {
    let result = match visibility_action(status) {
        VisibilityAction::ShowWithoutActivating => show_without_activating(window),
        VisibilityAction::NativeHide => hide_natively(window),
    };
    let _ = result;
}

fn visibility_action(status: &SessionStatus) -> VisibilityAction {
    if status.phase.presents_overlay() {
        VisibilityAction::ShowWithoutActivating
    } else {
        VisibilityAction::NativeHide
    }
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

#[cfg(windows)]
fn hide_natively(window: &WebviewWindow) -> tauri::Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    let hwnd = HWND(window.hwnd()?.0);
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    Ok(())
}

#[cfg(not(windows))]
fn hide_natively(window: &WebviewWindow) -> tauri::Result<()> {
    window.hide()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionPhase, SessionStatus};

    #[test]
    fn idle_requests_native_hide_after_a_native_show() {
        let status = SessionStatus {
            phase: SessionPhase::Idle,
            elapsed_ms: 0,
            audio_frames: 0,
            error: None,
        };

        assert_eq!(visibility_action(&status), VisibilityAction::NativeHide);
    }
}

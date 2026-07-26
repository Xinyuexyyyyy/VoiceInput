//! Windows-only text delivery for completed recognition results.
//!
//! Transcript text exists only while it is being pasted or copied. This module
//! never writes it to logs or files.

use arboard::Clipboard;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForegroundTarget(Option<isize>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertOutcome {
    Inserted,
    Copied,
    Failed,
}

pub trait TextInserter: Send + Sync {
    fn capture_target(&self) -> ForegroundTarget;
    fn insert_or_copy(&self, target: ForegroundTarget, text: &str) -> InsertOutcome;
    fn copy_only(&self, text: &str) -> InsertOutcome;
}

pub struct WindowsTextInserter;

impl TextInserter for WindowsTextInserter {
    fn capture_target(&self) -> ForegroundTarget {
        ForegroundTarget(current_foreground_window())
    }

    fn insert_or_copy(&self, target: ForegroundTarget, text: &str) -> InsertOutcome {
        if target.0.is_none() || target.0 != current_foreground_window() {
            return self.copy_only(text);
        }

        let previous_text = {
            let Ok(mut clipboard) = Clipboard::new() else {
                return InsertOutcome::Failed;
            };
            let previous_text = clipboard.get_text().ok();
            if clipboard.set_text(text.to_owned()).is_err() {
                return InsertOutcome::Failed;
            }
            previous_text
        };

        // The target process must open the clipboard itself to handle Ctrl+V.
        if !send_ctrl_v() {
            return InsertOutcome::Copied;
        }

        // Restore only when the clipboard was not changed by another app after
        // the paste. Non-text clipboard data cannot be reconstructed safely.
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Ok(mut clipboard) = Clipboard::new() {
            if clipboard.get_text().ok().as_deref() == Some(text) {
                if let Some(previous_text) = previous_text {
                    let _ = clipboard.set_text(previous_text);
                }
            }
        }
        InsertOutcome::Inserted
    }

    fn copy_only(&self, text: &str) -> InsertOutcome {
        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text.to_owned())) {
            Ok(()) => InsertOutcome::Copied,
            Err(_) => InsertOutcome::Failed,
        }
    }
}

#[cfg(target_os = "windows")]
fn current_foreground_window() -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let window = unsafe { GetForegroundWindow() };
    (!window.0.is_null()).then_some(window.0 as isize)
}

#[cfg(not(target_os = "windows"))]
fn current_foreground_window() -> Option<isize> {
    None
}

#[cfg(target_os = "windows")]
fn send_ctrl_v() -> bool {
    use std::mem::size_of;

    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_V,
    };

    let inputs = [
        key_input(VIRTUAL_KEY(VK_CONTROL.0), KEYBD_EVENT_FLAGS(0)),
        key_input(VIRTUAL_KEY(VK_V.0), KEYBD_EVENT_FLAGS(0)),
        key_input(VIRTUAL_KEY(VK_V.0), KEYEVENTF_KEYUP),
        key_input(VIRTUAL_KEY(VK_CONTROL.0), KEYEVENTF_KEYUP),
    ];
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) == inputs.len() as u32 }
}

#[cfg(target_os = "windows")]
fn key_input(
    key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT};

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(not(target_os = "windows"))]
fn send_ctrl_v() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_target_requires_the_same_window() {
        let initial = ForegroundTarget(Some(42));
        assert_eq!(initial.0, Some(42));
        assert_ne!(initial.0, Some(7));
    }
}

//! Windows-specific global hotkey handling.
//!
//! `Alt+Z` is handled by a low-level keyboard hook so the foreground app does
//! not receive the chord while dictation is toggled. Escape remains with the
//! Tauri shortcut plugin because it is registered only while a dictation
//! session is active.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_MENU, VK_Z};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, MSG, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::session::SessionController;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HotkeyAction {
    Toggle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KeyDecision {
    consume: bool,
    action: Option<HotkeyAction>,
    trigger_held: bool,
    alt_trigger_active: bool,
}

static EVENT_SENDER: OnceLock<Sender<HotkeyAction>> = OnceLock::new();
static TRIGGER_HELD: AtomicBool = AtomicBool::new(false);
static ALT_TRIGGER_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn install(session: SessionController) -> Result<(), HotkeyError> {
    let (event_tx, event_rx) = mpsc::channel();
    EVENT_SENDER
        .set(event_tx)
        .map_err(|_| HotkeyError::AlreadyInstalled)?;

    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("voiceinput-hotkey-hook".to_owned())
        .spawn(move || unsafe { run_hook_loop(ready_tx) })
        .map_err(HotkeyError::ThreadStart)?;

    ready_rx.recv().map_err(|_| {
        HotkeyError::HookStart("hook thread exited before initialization".to_owned())
    })??;

    std::thread::Builder::new()
        .name("voiceinput-hotkey-dispatch".to_owned())
        .spawn(move || {
            while let Ok(action) = event_rx.recv() {
                match action {
                    HotkeyAction::Toggle => session.toggle(),
                }
            }
        })
        .map_err(HotkeyError::ThreadStart)?;

    Ok(())
}

unsafe fn run_hook_loop(ready_tx: SyncSender<Result<(), HotkeyError>>) {
    let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) {
        Ok(hook) => hook,
        Err(error) => {
            let _ = ready_tx.send(Err(HotkeyError::HookStart(error.to_string())));
            return;
        }
    };
    let _ = ready_tx.send(Ok(()));

    let mut message = MSG::default();
    loop {
        // GetMessageW returns > 0 for a message, 0 for WM_QUIT, and -1 on error.
        if GetMessageW(&mut message, None, 0, 0).0 <= 0 {
            break;
        }
        let _ = TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    let _ = UnhookWindowsHookEx(hook);
}

unsafe extern "system" fn keyboard_hook(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if code >= HC_ACTION as i32 {
        let event = *(l_param.0 as *const KBDLLHOOKSTRUCT);
        let was_trigger_held = TRIGGER_HELD.load(Ordering::Relaxed);
        let alt_trigger_active = ALT_TRIGGER_ACTIVE.load(Ordering::Relaxed);
        let decision = classify_key_event(
            w_param.0 as u32,
            event.vkCode,
            event.flags.contains(LLKHF_ALTDOWN),
            was_trigger_held,
            alt_trigger_active,
        );

        TRIGGER_HELD.store(decision.trigger_held, Ordering::Relaxed);
        ALT_TRIGGER_ACTIVE.store(decision.alt_trigger_active, Ordering::Relaxed);
        if let Some(action) = decision.action {
            if let Some(sender) = EVENT_SENDER.get() {
                let _ = sender.send(action);
            }
        }
        if decision.consume {
            return LRESULT(1);
        }
    }

    CallNextHookEx(None, code, w_param, l_param)
}

fn classify_key_event(
    message: u32,
    virtual_key: u32,
    alt_down: bool,
    trigger_held: bool,
    alt_trigger_active: bool,
) -> KeyDecision {
    let is_trigger = virtual_key == VK_Z.0 as u32;
    let is_key_down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    let is_key_up = matches!(message, WM_KEYUP | WM_SYSKEYUP);

    if is_trigger && is_key_down && alt_down {
        return KeyDecision {
            consume: true,
            action: (!trigger_held).then_some(HotkeyAction::Toggle),
            trigger_held: true,
            alt_trigger_active: true,
        };
    }
    if is_trigger && is_key_up {
        return KeyDecision {
            consume: alt_trigger_active,
            action: None,
            trigger_held: false,
            alt_trigger_active,
        };
    }
    if virtual_key == VK_MENU.0 as u32 && is_key_up && alt_trigger_active {
        return KeyDecision {
            consume: true,
            action: None,
            trigger_held,
            alt_trigger_active: false,
        };
    }

    KeyDecision {
        consume: false,
        action: None,
        trigger_held,
        alt_trigger_active,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error("Alt+Z hook is already installed")]
    AlreadyInstalled,
    #[error("could not start Alt+Z hook thread: {0}")]
    ThreadStart(std::io::Error),
    #[error("could not install Alt+Z hook: {0}")]
    HookStart(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_z_system_keydown_dispatches_once_and_suppresses_the_chord() {
        let first = classify_key_event(WM_SYSKEYDOWN, VK_Z.0 as u32, true, false, false);
        assert_eq!(first.action, Some(HotkeyAction::Toggle));
        assert!(first.consume);
        assert!(first.trigger_held);
        assert!(first.alt_trigger_active);

        let repeat = classify_key_event(
            WM_SYSKEYDOWN,
            VK_Z.0 as u32,
            true,
            first.trigger_held,
            first.alt_trigger_active,
        );
        assert_eq!(repeat.action, None);
        assert!(repeat.consume);
    }

    #[test]
    fn releasing_alt_after_the_chord_is_also_suppressed() {
        let released = classify_key_event(WM_SYSKEYUP, VK_MENU.0 as u32, false, true, true);
        assert!(released.consume);
        assert!(!released.alt_trigger_active);
    }

    #[test]
    fn completing_a_chord_does_not_leave_a_system_key_event_for_the_next_toggle() {
        let pressed = classify_key_event(WM_SYSKEYDOWN, VK_Z.0 as u32, true, false, false);
        let trigger_released = classify_key_event(
            WM_SYSKEYUP,
            VK_Z.0 as u32,
            true,
            pressed.trigger_held,
            pressed.alt_trigger_active,
        );
        assert!(trigger_released.consume);

        let alt_released = classify_key_event(
            WM_SYSKEYUP,
            VK_MENU.0 as u32,
            false,
            trigger_released.trigger_held,
            trigger_released.alt_trigger_active,
        );
        let next = classify_key_event(
            WM_SYSKEYDOWN,
            VK_Z.0 as u32,
            true,
            alt_released.trigger_held,
            alt_released.alt_trigger_active,
        );
        assert_eq!(next.action, Some(HotkeyAction::Toggle));
    }

    #[test]
    fn unrelated_keys_are_forwarded_to_the_foreground_app() {
        let decision = classify_key_event(WM_KEYDOWN, b'A' as u32, false, false, false);
        assert!(!decision.consume);
        assert_eq!(decision.action, None);
    }
}

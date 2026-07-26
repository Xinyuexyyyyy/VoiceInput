//! Windows-specific global hotkey handling.
//!
//! `Alt+Space` is claimed by Windows for the foreground window's system menu,
//! so it cannot reliably use the ordinary `RegisterHotKey` route. A low-level
//! keyboard hook receives that system-key message before the foreground app
//! and consumes only this chord. Escape remains with the Tauri shortcut plugin
//! because it is registered only while a dictation session is active.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_MENU, VK_SPACE};
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
    space_held: bool,
    alt_space_active: bool,
}

static EVENT_SENDER: OnceLock<Sender<HotkeyAction>> = OnceLock::new();
static SPACE_HELD: AtomicBool = AtomicBool::new(false);
static ALT_SPACE_ACTIVE: AtomicBool = AtomicBool::new(false);

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
        let was_space_held = SPACE_HELD.load(Ordering::Relaxed);
        let alt_space_active = ALT_SPACE_ACTIVE.load(Ordering::Relaxed);
        let decision = classify_key_event(
            w_param.0 as u32,
            event.vkCode,
            event.flags.contains(LLKHF_ALTDOWN),
            was_space_held,
            alt_space_active,
        );

        SPACE_HELD.store(decision.space_held, Ordering::Relaxed);
        ALT_SPACE_ACTIVE.store(decision.alt_space_active, Ordering::Relaxed);
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
    space_held: bool,
    alt_space_active: bool,
) -> KeyDecision {
    let is_space = virtual_key == VK_SPACE.0 as u32;
    let is_key_down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    let is_key_up = matches!(message, WM_KEYUP | WM_SYSKEYUP);

    if is_space && is_key_down && alt_down {
        return KeyDecision {
            consume: true,
            action: (!space_held).then_some(HotkeyAction::Toggle),
            space_held: true,
            alt_space_active: true,
        };
    }
    if is_space && is_key_up {
        return KeyDecision {
            consume: false,
            action: None,
            space_held: false,
            alt_space_active,
        };
    }
    if virtual_key == VK_MENU.0 as u32 && is_key_up && alt_space_active {
        return KeyDecision {
            consume: true,
            action: None,
            space_held,
            alt_space_active: false,
        };
    }

    KeyDecision {
        consume: false,
        action: None,
        space_held,
        alt_space_active,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error("Alt+Space hook is already installed")]
    AlreadyInstalled,
    #[error("could not start Alt+Space hook thread: {0}")]
    ThreadStart(std::io::Error),
    #[error("could not install Alt+Space hook: {0}")]
    HookStart(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_space_system_keydown_dispatches_once_and_suppresses_the_system_menu() {
        let first = classify_key_event(WM_SYSKEYDOWN, VK_SPACE.0 as u32, true, false, false);
        assert_eq!(first.action, Some(HotkeyAction::Toggle));
        assert!(first.consume);
        assert!(first.space_held);
        assert!(first.alt_space_active);

        let repeat = classify_key_event(
            WM_SYSKEYDOWN,
            VK_SPACE.0 as u32,
            true,
            first.space_held,
            first.alt_space_active,
        );
        assert_eq!(repeat.action, None);
        assert!(repeat.consume);
    }

    #[test]
    fn releasing_alt_after_the_chord_is_also_suppressed() {
        let released = classify_key_event(WM_SYSKEYUP, VK_MENU.0 as u32, false, true, true);
        assert!(released.consume);
        assert!(!released.alt_space_active);
    }

    #[test]
    fn unrelated_keys_are_forwarded_to_the_foreground_app() {
        let decision = classify_key_event(WM_KEYDOWN, b'A' as u32, false, false, false);
        assert!(!decision.consume);
        assert_eq!(decision.action, None);
    }
}

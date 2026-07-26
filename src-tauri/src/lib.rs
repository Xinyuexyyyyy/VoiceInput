mod insertion;
mod session;

pub mod spike;

use std::sync::Arc;

use insertion::WindowsTextInserter;
use session::{SessionController, SessionStatus};
use tauri::{Emitter, Manager, State};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, ShortcutState};

#[derive(Clone)]
struct AppState {
    session: SessionController,
}

#[tauri::command]
fn session_status(state: State<'_, AppState>) -> SessionStatus {
    state.session.status()
}

#[tauri::command]
fn toggle_session(state: State<'_, AppState>) -> SessionStatus {
    state.session.toggle();
    state.session.status()
}

#[tauri::command]
fn cancel_session(state: State<'_, AppState>) -> SessionStatus {
    state.session.cancel();
    state.session.status()
}

pub fn run() {
    let session = SessionController::new(Arc::new(WindowsTextInserter));
    let shortcut_plugin = tauri_plugin_global_shortcut::Builder::new()
        .with_shortcuts(["ctrl+alt+space"])
        .expect("VoiceInput global shortcut registration could not be configured")
        .with_handler(|app, shortcut, event| {
            if event.state == ShortcutState::Pressed {
                let session = &app.state::<AppState>().session;
                if shortcut.matches(Modifiers::CONTROL | Modifiers::ALT, Code::Space) {
                    session.toggle();
                } else if shortcut.matches(Modifiers::empty(), Code::Escape) {
                    session.cancel();
                }
            }
        })
        .build();

    tauri::Builder::default()
        .manage(AppState {
            session: session.clone(),
        })
        .plugin(shortcut_plugin)
        .invoke_handler(tauri::generate_handler![
            session_status,
            toggle_session,
            cancel_session
        ])
        .setup(move |app| {
            let mut status_rx = session.subscribe_status();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while status_rx.changed().await.is_ok() {
                    let status = status_rx.borrow().clone();
                    let active = matches!(
                        status.phase,
                        session::SessionPhase::Starting
                            | session::SessionPhase::Listening
                            | session::SessionPhase::Finalizing
                    );
                    if active && !app_handle.global_shortcut().is_registered("esc") {
                        let _ = app_handle.global_shortcut().register("esc");
                    } else if !active && app_handle.global_shortcut().is_registered("esc") {
                        let _ = app_handle.global_shortcut().unregister("esc");
                    }
                    let _ = app_handle.emit("voiceinput://status", status);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("VoiceInput Tauri runtime failed");
}

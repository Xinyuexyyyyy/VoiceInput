pub mod spike;

pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("VoiceInput Tauri runtime failed");
}

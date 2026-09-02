#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Path of the clip to auto-load, so the end-to-end test can point the app at a generated file.
/// A normal app would let the user pick one instead.
#[tauri::command]
fn test_file() -> Option<String> {
    std::env::var("MPV_TEST_FILE").ok()
}

/// Lets the end-to-end test tell "the frontend never ran" apart from "the app never started",
/// which is otherwise invisible: both look like an empty log.
#[tauri::command]
fn frontend_ready(stage: String) {
    log::info!("frontend: {stage}");
}

fn main() {
    env_logger_init();
    log::info!("app starting");
    tauri::Builder::default()
        .plugin(tauri_plugin_mpv_surface::init())
        .invoke_handler(tauri::generate_handler![test_file, frontend_ready])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn env_logger_init() {
    // Deliberately dependency-free: just make plugin `log` output visible on stderr.
    struct Stderr;
    impl log::Log for Stderr {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
        fn flush(&self) {}
    }
    static LOGGER: Stderr = Stderr;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}

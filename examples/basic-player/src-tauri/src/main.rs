#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Path of the clip to auto-load, so the end-to-end test can point the app at a generated file.
/// A normal app would let the user pick one instead.
#[tauri::command]
fn test_file() -> Option<String> {
    std::env::var("MPV_TEST_FILE").ok()
}

fn main() {
    env_logger_init();
    tauri::Builder::default()
        .plugin(tauri_plugin_mpv::init())
        .invoke_handler(tauri::generate_handler![test_file])
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

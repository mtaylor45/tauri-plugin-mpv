const COMMANDS: &[&str] = &[
    "init",
    "destroy",
    "command",
    "set_property",
    "get_property",
    "observe_property",
    "unobserve_property",
    "set_video_rect",
    "set_surface_visible",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}

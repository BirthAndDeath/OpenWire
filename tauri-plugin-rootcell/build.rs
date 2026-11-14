const COMMANDS: &[&str] = &["is_keyring_available"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}

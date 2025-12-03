const COMMANDS: &[&str] = &["open_file", "close_file", "mmap_search", "read_lines", "get_total_lines"];

fn main() {
  tauri_plugin::Builder::new(COMMANDS)
    .build();
}

use tauri::{AppHandle, command, Runtime};

#[command]
pub(crate) async fn get_total_lines<R: Runtime>(_app: AppHandle<R>) -> std::result::Result<usize, String> {
    crate::models::get_total_lines().await
}

#[command]
pub(crate) async fn read_lines<R: Runtime>(_app: AppHandle<R>, start: usize, count: usize) -> std::result::Result<String, String> {
    crate::models::read_lines(start, count).await
}

#[command]
pub(crate) async fn mmap_search<R: Runtime>(_app: AppHandle<R>, needle: String, ignore_case: bool) -> std::result::Result<serde_json::Value, String> {
    crate::models::mmap_search(needle, ignore_case).await
}

#[command]
pub(crate) async fn close_file<R: Runtime>(_app: AppHandle<R>) -> std::result::Result<(), String> {
    crate::models::close_file().await
}

#[command]
pub(crate) async fn open_file<R: Runtime>(app: AppHandle<R>, extensions: Option<Vec<String>>) -> std::result::Result<serde_json::Value, String> {
    crate::models::open_file(app, extensions).await
}

#[command]
pub(crate) async fn get_file_size<R: Runtime>(_app: AppHandle<R>) -> std::result::Result<usize, String> {
    crate::models::get_file_size().await
}

use tauri::{
  plugin::{Builder, TauriPlugin},
  Runtime,
};

pub use models::*;

mod commands;
mod error;
mod models;

pub use error::{Error, Result};

/// Initializes the plugin.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
  Builder::new("large-file-preview")
    .invoke_handler(tauri::generate_handler![commands::get_total_lines,
                                           commands::read_lines,
                                           commands::mmap_search,
                                           commands::close_file,
                                           commands::open_file])
    .setup(|app, api| {
      Ok(())
    })
    .build()
}
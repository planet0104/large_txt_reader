// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // initialize logging for android (no-op on other platforms)
    #[cfg(target_os = "android")]
    {
           // initialize android logger with default config; adjust filters via env if needed
           android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info));
    }

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init());
    
        //子插件内不能注册，所以在这里注册
        #[cfg(target_os = "android")]
        let builder = builder.plugin(tauri_plugin_android_fs::init());
        
        let builder = builder.plugin(tauri_plugin_large_file_preview::init());
        
        builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

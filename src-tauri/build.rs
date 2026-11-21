fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&[
        "analyze_url",
        "get_settings",
        "set_options",
        "set_theme",
        "set_retries",
        "choose_folder",
        "ask_each_time",
        "choose_ffmpeg",
        "auto_ffmpeg",
        "app_info",
        "choose_download_folder",
        "clear_download_folder",
        "save_recording",
        "cancel_save",
        "open_in_loom",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("could not build the Tauri command manifest");
}

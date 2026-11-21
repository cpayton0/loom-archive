use loom_core::{
    AnalyzeError, CancellationToken, SaveError, SaveOptions, SaveRequest, TransferProgress,
    VideoDescriptor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Write as _,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{Manager, State, ipc::Channel};

#[tauri::command]
async fn analyze_url(url: String) -> Result<VideoDescriptor, AnalyzeError> {
    loom_core::analyze_url(&url).await
}

/// Persistent defaults. Paths are only ever set through native pickers, so the
/// WebView cannot point a save or an executable at an arbitrary path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Theme {
    #[default]
    Dark,
    Light,
    /// Follow the Windows app mode.
    System,
}

/// Matches the native title bar and the pre-paint window color to the theme.
fn apply_theme<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>, theme: Theme) {
    let (native, background) = match theme {
        Theme::Dark => (Some(tauri::Theme::Dark), Some((0x11, 0x12, 0x14))),
        Theme::Light => (Some(tauri::Theme::Light), Some((0xfa, 0xfa, 0xf9))),
        Theme::System => (None, None),
    };
    let _ = window.set_theme(native);
    let _ = window.set_background_color(
        background.map(|(r, g, b)| tauri::window::Color(r, g, b, 255)),
    );
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    theme: Theme,
    folder: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    retries: u8,
    /// Batch downloads running at once (upstream uses 5).
    concurrency: u8,
    /// Pause after each batch download (upstream `--timeout`, default 5 s).
    batch_delay_secs: u16,
    /// Skip links already in the download history (upstream `downloaded.log`).
    skip_downloaded: bool,
    #[serde(flatten)]
    options: SaveOptions,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            folder: None,
            ffmpeg: None,
            retries: 0,
            concurrency: 2,
            batch_delay_secs: 5,
            skip_downloaded: true,
            options: SaveOptions::default(),
        }
    }
}

impl Settings {
    /// Brings hand-edited or old values back into the supported ranges.
    fn normalized(mut self) -> Self {
        let (c, d, skip) = (self.concurrency, self.batch_delay_secs, self.skip_downloaded);
        apply_batch(&mut self, c, d, skip);
        self.retries = self.retries.min(MAX_RETRIES);
        self
    }
}

fn apply_batch(s: &mut Settings, concurrency: u8, delay_secs: u16, skip_downloaded: bool) {
    s.concurrency = concurrency.clamp(1, MAX_CONCURRENCY);
    s.batch_delay_secs = delay_secs.min(MAX_BATCH_DELAY);
    s.skip_downloaded = skip_downloaded;
}

const MAX_RETRIES: u8 = 3;
const MAX_CONCURRENCY: u8 = 5;
const MAX_BATCH_DELAY: u16 = 600;

struct SettingsStore {
    path: Option<PathBuf>,
    current: Mutex<Settings>,
}

impl SettingsStore {
    fn load(path: Option<PathBuf>) -> Self {
        let current = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .unwrap_or_default()
            .normalized();
        Self {
            path,
            current: Mutex::new(current),
        }
    }

    fn get(&self) -> Result<Settings, String> {
        self.current
            .lock()
            .map(|s| s.clone())
            .map_err(|_| "Settings are unavailable.".into())
    }

    fn update(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let mut current = self
            .current
            .lock()
            .map_err(|_| "Settings are unavailable.")?;
        let mut next = current.clone();
        change(&mut next);
        if let Some(path) = &self.path {
            write_settings(path, &next).map_err(|_| "Could not save settings.")?;
        }
        *current = next.clone();
        Ok(next)
    }
}

fn write_settings(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let dir = path.parent().ok_or(std::io::ErrorKind::NotFound)?;
    std::fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(&mut temp, settings)?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

async fn pick_folder<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    title: &str,
) -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_parent(window)
        .set_title(title)
        .pick_folder()
        .await
        .map(|f| f.path().to_owned())
}

#[tauri::command]
fn get_settings(store: State<'_, SettingsStore>) -> Result<Settings, String> {
    store.get()
}

#[tauri::command]
fn set_options(store: State<'_, SettingsStore>, options: SaveOptions) -> Result<Settings, String> {
    store.update(|s| s.options = options)
}

#[tauri::command]
fn set_theme<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    store: State<'_, SettingsStore>,
    theme: Theme,
) -> Result<Settings, String> {
    let settings = store.update(|s| s.theme = theme)?;
    apply_theme(&window, theme);
    Ok(settings)
}

#[tauri::command]
fn set_batch(
    store: State<'_, SettingsStore>,
    concurrency: u8,
    delay_secs: u16,
    skip_downloaded: bool,
) -> Result<Settings, String> {
    store.update(|s| apply_batch(s, concurrency, delay_secs, skip_downloaded))
}

/// IDs of recordings saved before, appended to `downloaded.log` like upstream.
struct History {
    path: Option<PathBuf>,
    ids: Mutex<HashSet<String>>,
}

impl History {
    fn load(path: Option<PathBuf>) -> Self {
        let ids = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|l| l.len() == 32 && l.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(str::to_ascii_lowercase)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path,
            ids: Mutex::new(ids),
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.ids.lock().is_ok_and(|ids| ids.contains(id))
    }

    fn record(&self, id: &str) {
        let Ok(mut ids) = self.ids.lock() else { return };
        if !ids.insert(id.to_ascii_lowercase()) {
            return;
        }
        if let Some(path) = &self.path {
            let _ = path.parent().map(std::fs::create_dir_all);
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{id}");
            }
        }
    }
}

/// For each link, whether its recording is already in the download history.
/// Links that are not valid share links report `false`.
#[tauri::command]
fn downloaded(history: State<'_, History>, urls: Vec<String>) -> Vec<bool> {
    downloaded_flags(&history, &urls)
}

fn downloaded_flags(history: &History, urls: &[String]) -> Vec<bool> {
    urls.iter()
        .map(|url| {
            loom_core::normalize_share_url(url)
                .ok()
                .and_then(|u| u.path().strip_prefix("/share/").map(str::to_owned))
                .is_some_and(|id| history.contains(&id))
        })
        .collect()
}

const MAX_LIST_BYTES: u64 = 1024 * 1024;
const MAX_LIST_LINES: usize = 1000;

/// Upstream's `--list`: a text file with one link per line, read in Rust so
/// the WebView never handles file paths.
#[tauri::command]
async fn open_link_list<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
) -> Result<Option<Vec<String>>, String> {
    let picked = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Open a list of Loom links")
        .add_filter("Text files", &["txt"])
        .pick_file()
        .await;
    let Some(file) = picked else { return Ok(None) };
    let path = file.path().to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let meta = std::fs::metadata(&path).map_err(|_| "Could not read that file.")?;
        if meta.len() > MAX_LIST_BYTES {
            return Err("That list is larger than 1 MB.".to_string());
        }
        let text = std::fs::read_to_string(&path).map_err(|_| "The list must be a UTF-8 text file.")?;
        Ok(Some(parse_link_list(&text)))
    })
    .await
    .map_err(|_| "Could not read that file.".to_string())?
}

fn parse_link_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .take(MAX_LIST_LINES)
        .map(String::from)
        .collect()
}

#[tauri::command]
fn set_retries(store: State<'_, SettingsStore>, retries: u8) -> Result<Settings, String> {
    store.update(|s| s.retries = retries.min(MAX_RETRIES))
}

#[tauri::command]
async fn choose_folder<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    store: State<'_, SettingsStore>,
) -> Result<Settings, String> {
    match pick_folder(&window, "Choose the default download folder").await {
        Some(folder) => store.update(|s| s.folder = Some(folder)),
        None => store.get(),
    }
}

#[tauri::command]
fn ask_each_time(store: State<'_, SettingsStore>) -> Result<Settings, String> {
    store.update(|s| s.folder = None)
}

#[tauri::command]
async fn choose_ffmpeg<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    store: State<'_, SettingsStore>,
) -> Result<Settings, String> {
    let picked = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Choose the FFmpeg executable")
        .add_filter("FFmpeg", &[if cfg!(windows) { "exe" } else { "" }])
        .pick_file()
        .await;
    let Some(file) = picked else {
        return store.get();
    };
    let path = file.path().to_owned();
    let named_ffmpeg = path
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("ffmpeg"));
    if !named_ffmpeg || !path.is_file() {
        return Err("Choose the ffmpeg executable (ffmpeg.exe).".into());
    }
    store.update(|s| s.ffmpeg = Some(path))
}

#[tauri::command]
fn auto_ffmpeg(store: State<'_, SettingsStore>) -> Result<Settings, String> {
    store.update(|s| s.ffmpeg = None)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    ffmpeg: Option<String>,
}

#[tauri::command]
fn app_info<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, SettingsStore>,
) -> Result<AppInfo, String> {
    let settings = store.get()?;
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        ffmpeg: loom_core::find_ffmpeg(settings.ffmpeg.as_deref()).map(|p| p.display().to_string()),
    })
}

/// A destination chosen for the current download only. Never persisted.
#[derive(Default)]
struct DownloadFolder(Mutex<Option<PathBuf>>);

#[tauri::command]
async fn choose_download_folder<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    folder: State<'_, DownloadFolder>,
) -> Result<Option<String>, String> {
    let picked = pick_folder(&window, "Save this download to…").await;
    let mut slot = folder.0.lock().map_err(|_| "Folder selection is unavailable.")?;
    if picked.is_some() {
        *slot = picked;
    }
    Ok(slot.as_ref().map(|p| p.display().to_string()))
}

/// Fixes the destination for a batch: keeps a folder chosen for this
/// download, otherwise copies the current default, so later changes in
/// Settings cannot split one batch across folders. Returns the folder, or
/// `None` when there is no default (the UI then asks once).
#[tauri::command]
fn pin_download_folder(
    folder: State<'_, DownloadFolder>,
    store: State<'_, SettingsStore>,
) -> Result<Option<String>, String> {
    let default = store.get()?.folder;
    let mut slot = folder.0.lock().map_err(|_| "Folder selection is unavailable.")?;
    if slot.is_none() {
        *slot = default;
    }
    Ok(slot.as_ref().map(|p| p.display().to_string()))
}

#[tauri::command]
fn clear_download_folder(folder: State<'_, DownloadFolder>) -> Result<(), String> {
    *folder.0.lock().map_err(|_| "Folder selection is unavailable.")? = None;
    Ok(())
}

/// Active downloads by the job number the UI assigned.
#[derive(Default)]
struct Downloads(Mutex<HashMap<u32, CancellationToken>>);

struct ActiveSave<'a>(&'a Downloads, u32);

impl Drop for ActiveSave<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.0.lock() {
            active.remove(&self.1);
        }
    }
}

#[derive(Serialize)]
struct SaveResult {
    files: Vec<String>,
    notes: Vec<String>,
}

/// Per-download choices sent by the UI. They are combined with the stored
/// defaults here and never written back to settings.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadRequest {
    options: SaveOptions,
    file_name: Option<String>,
    /// Batch numbering: `<prefix>-<n>-<name>`.
    prefix: Option<String>,
    index: Option<usize>,
}

#[tauri::command]
async fn save_recording<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, Downloads>,
    store: State<'_, SettingsStore>,
    override_folder: State<'_, DownloadFolder>,
    history: State<'_, History>,
    job: u32,
    url: String,
    request: DownloadRequest,
    progress: Channel<TransferProgress>,
) -> Result<Option<SaveResult>, String> {
    let share = loom_core::normalize_share_url(&url).map_err(|e| e.to_string())?;
    let settings = store.get()?;
    let chosen = override_folder
        .0
        .lock()
        .map_err(|_| "Folder selection is unavailable.")?
        .clone();
    let token = CancellationToken::new();
    let _active = state.start(job, token.clone())?;
    let folder = match chosen.or(settings.folder) {
        Some(folder) if folder.is_dir() => folder,
        Some(_) => return Err("The destination folder no longer exists. Choose another.".into()),
        None => match pick_folder(&window, "Choose an archive folder").await {
            Some(folder) => folder,
            None => return Ok(None),
        },
    };
    let request = SaveRequest {
        options: request.options,
        file_name: request.file_name,
        ffmpeg: settings.ffmpeg,
        retries: settings.retries.min(MAX_RETRIES),
        prefix: batch_prefix(request.prefix, request.index),
    };
    let notify = |value| {
        if progress.send(value).is_err() {
            token.cancel();
        }
    };
    notify(TransferProgress {
        bytes: 0,
        total: None,
        percent: None,
        finalizing: false,
    });
    match loom_core::save_recording(share.as_ref(), &folder, &request, &token, notify).await {
        Ok(saved) => Ok(Some({
            history.record(&saved.id);
            SaveResult {
            files: saved
                .files
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            notes: saved.notes,
        }})),
        Err(SaveError::Canceled) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

impl Downloads {
    /// Registers a running job; the guard unregisters it when dropped.
    fn start(&self, job: u32, token: CancellationToken) -> Result<ActiveSave<'_>, String> {
        let mut slot = self
            .0
            .lock()
            .map_err(|_| "The download manager is unavailable.")?;
        if slot.contains_key(&job) {
            return Err("That download is already running.".into());
        }
        if slot.len() >= usize::from(MAX_CONCURRENCY) {
            return Err("Too many downloads are running. Wait for one to finish.".into());
        }
        slot.insert(job, token);
        Ok(ActiveSave(self, job))
    }

    fn cancel(&self, job: Option<u32>) -> Result<(), String> {
        let active = self
            .0
            .lock()
            .map_err(|_| "The download manager is unavailable.")?;
        match job {
            Some(job) => active.get(&job).into_iter().for_each(CancellationToken::cancel),
            None => active.values().for_each(CancellationToken::cancel),
        }
        Ok(())
    }
}

/// Cancels one job, or every running download when `job` is omitted.
#[tauri::command]
fn cancel_save(state: State<'_, Downloads>, job: Option<u32>) -> Result<(), String> {
    state.cancel(job)
}

/// Batch numbering from the UI; a blank prefix means no numbering.
fn batch_prefix(prefix: Option<String>, index: Option<usize>) -> Option<(String, usize)> {
    prefix.filter(|p| !p.trim().is_empty()).zip(index)
}

#[tauri::command]
async fn open_in_loom(url: String) -> Result<(), String> {
    let page = loom_core::normalize_share_url(&url).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || open::that(page.as_str()))
        .await
        .map_err(|_| "Could not launch the default browser.")?
        .map_err(|_| "Could not open Loom in the default browser.".into())
}

fn with_commands<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(Downloads::default())
        .manage(DownloadFolder::default())
        .setup(|app| {
            let path = app
                .path()
                .app_config_dir()
                .ok()
                .map(|dir| dir.join("settings.json"));
            let history = app
                .path()
                .app_data_dir()
                .ok()
                .map(|dir| dir.join("downloaded.log"));
            app.manage(History::load(history));
            let store = SettingsStore::load(path);
            if let (Some(window), Ok(settings)) = (app.get_webview_window("main"), store.get()) {
                apply_theme(&window, settings.theme);
            }
            app.manage(store);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            analyze_url,
            get_settings,
            set_options,
            set_theme,
            set_retries,
            set_batch,
            downloaded,
            open_link_list,
            choose_folder,
            ask_each_time,
            choose_ffmpeg,
            auto_ffmpeg,
            app_info,
            choose_download_folder,
            pin_download_folder,
            clear_download_folder,
            save_recording,
            cancel_save,
            open_in_loom
        ])
}

pub fn run() {
    with_commands(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("could not start Loom Archive");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tauri::{WebviewWindowBuilder, ipc::CallbackFn, test, webview::InvokeRequest};

    #[test]
    fn settings_round_trip_and_tolerate_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let store = SettingsStore::load(Some(path.clone()));
        assert!(store.get().unwrap().folder.is_none());
        store
            .update(|s| {
                s.options.transcript = true;
                s.options.quality = loom_core::Quality::P720;
                s.retries = 2;
                s.theme = Theme::Light;
            })
            .unwrap();
        let reloaded = SettingsStore::load(Some(path.clone())).get().unwrap();
        assert!(reloaded.options.transcript);
        assert_eq!(reloaded.options.quality, loom_core::Quality::P720);
        assert_eq!(reloaded.retries, 2);
        assert_eq!(reloaded.theme, Theme::Light);
        std::fs::write(&path, r#"{"thumbnail":true}"#).unwrap();
        let partial = SettingsStore::load(Some(path)).get().unwrap();
        assert!(partial.options.thumbnail && !partial.options.transcript);
    }

    #[test]
    fn history_survives_restarts_and_ignores_junk_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("downloaded.log");
        let id = "0123456789abcdef0123456789abcdef";
        let history = History::load(Some(path.clone()));
        assert!(!history.contains(id));
        history.record(id);
        history.record(id);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not an id\n")
            .unwrap();
        let reloaded = History::load(Some(path.clone()));
        assert!(reloaded.contains(id));
        assert!(!reloaded.contains("not an id"));
        assert_eq!(reloaded.ids.lock().unwrap().len(), 1);
        assert_eq!(std::fs::read_to_string(path).unwrap().matches(id).count(), 1);
    }

    #[test]
    fn link_lists_skip_blanks_and_comments_and_are_capped() {
        let text = "# exported links\n\n  https://loom.com/share/a  \nhttps://loom.com/share/b\r\n";
        assert_eq!(parse_link_list(text), ["https://loom.com/share/a", "https://loom.com/share/b"]);
        let many = "x\n".repeat(MAX_LIST_LINES + 10);
        assert_eq!(parse_link_list(&many).len(), MAX_LIST_LINES);
    }

    #[test]
    fn old_settings_files_get_batch_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"retries":1}"#).unwrap();
        assert_eq!((settings.concurrency, settings.batch_delay_secs, settings.skip_downloaded), (2, 5, true));
    }

    #[test]
    fn jobs_are_capped_unique_and_cancelled_individually_or_together() {
        let downloads = Downloads::default();
        let tokens: Vec<_> = (0..5).map(|_| CancellationToken::new()).collect();
        let mut guards: Vec<_> = tokens
            .iter()
            .enumerate()
            .map(|(i, t)| downloads.start(i as u32, t.clone()).unwrap())
            .collect();
        assert!(downloads.start(9, CancellationToken::new()).is_err(), "cap");
        drop(guards.pop()); // job 4 finishes
        assert!(downloads.start(0, CancellationToken::new()).is_err(), "duplicate job");
        let fifth = downloads.start(4, tokens[4].clone()).unwrap();
        downloads.cancel(Some(1)).unwrap();
        assert!(tokens[1].is_cancelled() && !tokens[0].is_cancelled());
        downloads.cancel(None).unwrap();
        assert!(tokens.iter().all(CancellationToken::is_cancelled));
        drop(fifth);
        assert_eq!(downloads.0.lock().unwrap().len(), 4, "only the dropped guard unregisters");
        drop(guards);
        assert!(downloads.0.lock().unwrap().is_empty());
    }

    #[test]
    fn downloaded_flags_match_any_link_form_of_a_saved_recording() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::load(Some(dir.path().join("downloaded.log")));
        history.record("0123456789ABCDEF0123456789ABCDEF");
        let urls = [
            "https://www.loom.com/share/0123456789abcdef0123456789abcdef",
            "https://loom.com/embed/0123456789ABCDEF0123456789ABCDEF?t=1",
            "https://www.loom.com/share/Demo-0123456789abcdef0123456789abcdef",
            "https://www.loom.com/share/fedcba9876543210fedcba9876543210",
            "not a link",
        ]
        .map(String::from);
        assert_eq!(downloaded_flags(&history, &urls), [true, true, true, false, false]);
    }

    #[test]
    fn batch_settings_are_clamped_wherever_they_come_from() {
        let mut settings = Settings::default();
        apply_batch(&mut settings, 0, 9999, false);
        assert_eq!((settings.concurrency, settings.batch_delay_secs, settings.skip_downloaded), (1, MAX_BATCH_DELAY, false));
        apply_batch(&mut settings, 9, 5, true);
        assert_eq!(settings.concurrency, MAX_CONCURRENCY);
        let edited: Settings = serde_json::from_str(r#"{"concurrency":0,"batchDelaySecs":65535,"retries":40}"#).unwrap();
        let normalized = edited.normalized();
        assert_eq!((normalized.concurrency, normalized.batch_delay_secs, normalized.retries), (1, MAX_BATCH_DELAY, MAX_RETRIES));
        assert_eq!(batch_prefix(Some("  ".into()), Some(1)), None);
        assert_eq!(batch_prefix(Some("demo".into()), None), None);
        assert_eq!(batch_prefix(Some("demo".into()), Some(2)), Some(("demo".into(), 2)));
    }

    #[test]
    fn only_the_main_window_can_call_the_analyzer() {
        let app = with_commands(test::mock_builder())
            .build(tauri::generate_context!())
            .unwrap();
        // The mock app does not run setup; register the state `downloaded` reads.
        app.manage(History::load(None));
        for label in ["main", "other"] {
            let window = WebviewWindowBuilder::new(&app, label, Default::default())
                .build()
                .unwrap();
            let response = test::get_ipc_response(
                &window,
                InvokeRequest {
                    cmd: "analyze_url".into(),
                    callback: CallbackFn(0),
                    error: CallbackFn(1),
                    url: tauri::Manager::config(&app)
                        .build
                        .dev_url
                        .clone()
                        .unwrap_or_else(|| "http://tauri.localhost".parse().unwrap()),
                    body: json!({"url":"https://example.invalid/share/test"}).into(),
                    headers: Default::default(),
                    invoke_key: test::INVOKE_KEY.into(),
                },
            );
            let error = response.expect_err("invalid input must be rejected");
            if label == "main" {
                assert_eq!(error["code"], "invalidUrl", "{error}");
                assert_eq!(error["message"], AnalyzeError::InvalidUrl.to_string());
            } else {
                assert!(
                    error.as_str().is_some_and(|s| s.contains("not allowed")),
                    "{error}"
                );
            }
            // Batch commands: granted to the main window only. Only `downloaded`
            // is invoked from main, because the others write settings or open dialogs.
            for (cmd, body) in [
                ("downloaded", json!({"urls": []})),
                ("set_batch", json!({"concurrency": 1, "delaySecs": 0, "skipDownloaded": true})),
                ("open_link_list", json!({})),
                ("pin_download_folder", json!({})),
            ] {
                if label == "main" && cmd != "downloaded" {
                    continue;
                }
                let response = test::get_ipc_response(
                    &window,
                    InvokeRequest {
                        cmd: cmd.into(),
                        callback: CallbackFn(0),
                        error: CallbackFn(1),
                        url: tauri::Manager::config(&app)
                            .build
                            .dev_url
                            .clone()
                            .unwrap_or_else(|| "http://tauri.localhost".parse().unwrap()),
                        body: body.into(),
                        headers: Default::default(),
                        invoke_key: test::INVOKE_KEY.into(),
                    },
                );
                if label == "main" {
                    assert!(response.is_ok(), "{cmd}: {response:?}");
                } else {
                    let error = response.expect_err("other windows are denied");
                    assert!(error.as_str().is_some_and(|s| s.contains("not allowed")), "{cmd}: {error}");
                }
            }
        }
    }
}

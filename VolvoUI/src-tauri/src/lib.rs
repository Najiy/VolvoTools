//! Tauri shell: wires the volvo-core backend to the webview.
//!
//! Commands are thin. Every safety decision is made inside volvo-core, so a bug or a modified
//! front end cannot skip a gate.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use volvo_core::events::{EventSink, JobEvent, JobSnapshot};
use volvo_core::flasher::{FLASHER_ENV, ScanResult};
use volvo_core::history::HistoryEntry;
use volvo_core::library::BackupEntry;
use volvo_core::profile::Profile;
use volvo_core::settings::Settings;
use volvo_core::{
    CONFIRM_PHRASE, Core, CoreError, CoreOptions, Dirs, FlashPreview, FlashRequest,
    PREFLIGHT_ITEMS, events::JobInfo,
};

const JOB_EVENT: &str = "job-event";
const CLOSE_BLOCKED_EVENT: &str = "close-blocked";

type Reply<T> = Result<T, String>;

fn text(error: CoreError) -> String {
    error.to_string()
}

/// Forwards job events to the webview.
struct WebviewSink(AppHandle);

impl EventSink for WebviewSink {
    fn emit(&self, event: &JobEvent) {
        let _ = self.0.emit(JOB_EVENT, event);
    }
}

fn sink(app: &AppHandle) -> Arc<dyn EventSink> {
    Arc::new(WebviewSink(app.clone()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: &'static str,
    profile: &'static Profile,
    default_backups_dir: String,
    log_dir: String,
    confirm_phrase: &'static str,
    preflight_items: &'static [&'static str],
    sector_size: usize,
}

#[tauri::command]
fn app_info(core: State<'_, Core>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        profile: core.profile(),
        default_backups_dir: core.default_backups_dir().display().to_string(),
        log_dir: core.log_dir().display().to_string(),
        confirm_phrase: CONFIRM_PHRASE,
        preflight_items: &PREFLIGHT_ITEMS,
        sector_size: volvo_core::profile::SECTOR,
    }
}

#[tauri::command]
fn get_settings(core: State<'_, Core>) -> Settings {
    core.settings()
}

#[tauri::command]
fn save_settings(core: State<'_, Core>, settings: Settings) -> Reply<Settings> {
    core.save_settings(settings).map_err(text)
}

/// Runs blocking core work off the command thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
) -> Reply<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("Internal error: {e}"))?
        .map_err(text)
}

#[tauri::command]
async fn scan(core: State<'_, Core>) -> Reply<ScanResult> {
    let core = core.inner().clone();
    blocking(move || core.scan()).await
}

#[tauri::command]
fn list_backups(core: State<'_, Core>) -> Vec<BackupEntry> {
    core.backups()
}

#[tauri::command]
fn list_history(core: State<'_, Core>) -> Vec<HistoryEntry> {
    core.history(50)
}

#[tauri::command]
fn current_job(core: State<'_, Core>) -> JobSnapshot {
    core.snapshot()
}

#[tauri::command]
async fn preview_flash(
    core: State<'_, Core>,
    tune_path: String,
    backup_id: String,
) -> Reply<FlashPreview> {
    let core = core.inner().clone();
    blocking(move || core.preview_flash(&tune_path, &backup_id)).await
}

#[tauri::command]
async fn start_backup(
    app: AppHandle,
    core: State<'_, Core>,
    device: String,
    verify: bool,
) -> Reply<JobInfo> {
    let core = core.inner().clone();
    let sink = sink(&app);
    blocking(move || core.start_backup(sink, device, verify)).await
}

#[tauri::command]
async fn start_flash(
    app: AppHandle,
    core: State<'_, Core>,
    request: FlashRequest,
) -> Reply<JobInfo> {
    let core = core.inner().clone();
    let sink = sink(&app);
    blocking(move || core.start_flash(sink, request)).await
}

#[tauri::command]
fn cancel_job(core: State<'_, Core>, force: bool) -> Reply<()> {
    core.cancel(force).map_err(text)
}

#[tauri::command]
async fn export_backup(core: State<'_, Core>, id: String, destination: String) -> Reply<()> {
    let core = core.inner().clone();
    blocking(move || core.export_backup(&id, Path::new(&destination))).await
}

fn open_in_explorer(arguments: &[String]) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("explorer.exe")
            .raw_arg(arguments.join(" "))
            .spawn();
    }
    #[cfg(not(windows))]
    let _ = arguments;
}

/// Shows a backup's image in Explorer. The path is rebuilt from the id, never taken from the UI.
#[tauri::command]
fn reveal_backup(core: State<'_, Core>, id: String) -> Reply<()> {
    let entry = core.library().get(&id).map_err(text)?;
    open_in_explorer(&[format!("/select,\"{}\"", entry.path)]);
    Ok(())
}

#[tauri::command]
fn reveal_backups(core: State<'_, Core>) -> Reply<()> {
    let dir = core.backups_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    open_in_explorer(&[format!("\"{}\"", dir.display())]);
    Ok(())
}

#[tauri::command]
fn reveal_logs(core: State<'_, Core>) {
    open_in_explorer(&[format!("\"{}\"", core.log_dir().display())]);
}

/// Keeps all state under one folder when `VOLVOUI_HOME` is set, so a simulator session can never
/// put fake backups next to real ones.
const HOME_ENV: &str = "VOLVOUI_HOME";

fn build_core(app: &tauri::App) -> Result<Core, Box<dyn std::error::Error>> {
    let paths = app.path();
    let dirs = match std::env::var_os(HOME_ENV).filter(|value| !value.is_empty()) {
        Some(home) => {
            let home = PathBuf::from(home);
            Dirs {
                settings_file: home.join("settings.json"),
                data_dir: home.join("data"),
                default_backups_dir: home.join("backups"),
            }
        }
        None => Dirs {
            settings_file: paths.app_config_dir()?.join("settings.json"),
            data_dir: paths.app_local_data_dir()?,
            default_backups_dir: paths.document_dir()?.join("VolvoTools").join("Backups"),
        },
    };
    let exe_dir: Option<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let options = CoreOptions {
        flasher_override: std::env::var_os(FLASHER_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        ..CoreOptions::default()
    };
    Ok(Core::new(dirs, exe_dir, options))
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let core = build_core(app)?;
            app.manage(core);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window mid-job would leave the ECU in whatever state the job was in.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.state::<Core>().is_busy()
            {
                api.prevent_close();
                let _ = window.emit(CLOSE_BLOCKED_EVENT, ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            get_settings,
            save_settings,
            scan,
            list_backups,
            list_history,
            current_job,
            preview_flash,
            start_backup,
            start_flash,
            cancel_job,
            export_backup,
            reveal_backup,
            reveal_backups,
            reveal_logs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the VolvoTools UI");
}

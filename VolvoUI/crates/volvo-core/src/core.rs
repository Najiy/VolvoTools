//! The facade the Tauri layer talks to: settings, discovery, previews and starting jobs.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::backup::run_backup;
use crate::error::{CoreError, Result, io_err};
use crate::events::{EventSink, JobInfo, JobKind, JobSnapshot};
use crate::flash::{
    AssessCtx, CONFIRM_PHRASE, FlashPreview, FlashRequest, PREFLIGHT_ITEMS, assess, run_flash,
};
use crate::flasher::{self, ScanResult, Source};
use crate::history::{self, HistoryEntry};
use crate::jobs::{self, Env, Shared};
use crate::library::{BackupEntry, Library};
use crate::profile::{ME9_P1, Profile};
use crate::settings::{Settings, SettingsStore};

const RUN_DIR_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);

#[derive(Debug, Clone)]
pub struct Dirs {
    pub settings_file: PathBuf,
    /// Holds `runs/` (flasher working directories and logs) and `history.jsonl`.
    pub data_dir: PathBuf,
    pub default_backups_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CoreOptions {
    /// Pause between the two reads of a backup so the car's network can settle.
    pub pass_gap: Duration,
    /// Forces this flasher regardless of the saved setting (see `flasher::FLASHER_ENV`).
    pub flasher_override: Option<PathBuf>,
    /// Extra environment variables for every flasher process.
    pub flasher_env: Vec<(String, String)>,
}

impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            pass_gap: Duration::from_secs(8),
            flasher_override: None,
            flasher_env: Vec::new(),
        }
    }
}

struct Inner {
    dirs: Dirs,
    store: SettingsStore,
    settings: Mutex<Settings>,
    shared: Arc<Shared>,
    app_exe_dir: Option<PathBuf>,
    options: CoreOptions,
}

#[derive(Clone)]
pub struct Core(Arc<Inner>);

impl Core {
    pub fn new(dirs: Dirs, app_exe_dir: Option<PathBuf>, options: CoreOptions) -> Self {
        let store = SettingsStore::new(dirs.settings_file.clone());
        let settings = Mutex::new(store.load());
        let core = Core(Arc::new(Inner {
            dirs,
            store,
            settings,
            shared: Arc::new(Shared::new()),
            app_exe_dir,
            options,
        }));
        core.prune_runs();
        core
    }

    pub fn profile(&self) -> &'static Profile {
        &ME9_P1
    }

    pub fn settings(&self) -> Settings {
        self.0.settings.lock().unwrap().clone()
    }

    pub fn save_settings(&self, settings: Settings) -> Result<Settings> {
        let settings = settings.normalized();
        self.0.store.save(&settings)?;
        *self.0.settings.lock().unwrap() = settings.clone();
        Ok(settings)
    }

    pub fn default_backups_dir(&self) -> &Path {
        &self.0.dirs.default_backups_dir
    }

    pub fn backups_dir(&self) -> PathBuf {
        match self.settings().backups_dir {
            Some(dir) => PathBuf::from(dir),
            None => self.0.dirs.default_backups_dir.clone(),
        }
    }

    pub fn log_dir(&self) -> PathBuf {
        self.0.dirs.data_dir.join("runs")
    }

    pub fn library(&self) -> Library {
        Library::new(self.backups_dir())
    }

    pub fn flasher(&self) -> Option<(PathBuf, Source)> {
        flasher::resolve(
            self.0.options.flasher_override.as_deref(),
            self.settings().flasher_path.as_deref(),
            self.0.app_exe_dir.as_deref(),
        )
    }

    fn scratch_dir(&self, name: &str) -> Result<PathBuf> {
        let dir = self.log_dir().join(name);
        fs::create_dir_all(&dir).map_err(io_err(format!("creating {}", dir.display())))?;
        Ok(dir)
    }

    /// Finds the flasher and lists the J2534 devices it can see.
    pub fn scan(&self) -> Result<ScanResult> {
        let scratch = self.scratch_dir("probe")?;
        Ok(flasher::probe(
            self.flasher(),
            &scratch,
            &self.0.options.flasher_env,
        ))
    }

    pub fn backups(&self) -> Vec<BackupEntry> {
        self.library().list()
    }

    pub fn export_backup(&self, id: &str, destination: &Path) -> Result<()> {
        let entry = self.library().get(id)?;
        fs::copy(&entry.path, destination)
            .map(|_| ())
            .map_err(io_err(format!("copying to {}", destination.display())))
    }

    pub fn history(&self, limit: usize) -> Vec<HistoryEntry> {
        history::recent(&self.history_path(), limit)
    }

    pub fn snapshot(&self) -> JobSnapshot {
        self.0.shared.snapshot()
    }

    pub fn is_busy(&self) -> bool {
        self.0.shared.is_busy()
    }

    pub fn cancel(&self, force: bool) -> Result<()> {
        self.0.shared.request_cancel(force)
    }

    fn history_path(&self) -> PathBuf {
        self.0.dirs.data_dir.join("history.jsonl")
    }

    /// Creates the working directory of one job.
    fn make_env(&self, exe: PathBuf, label: &str) -> Result<Env> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f");
        let run_dir = self.log_dir().join(format!("{stamp}-{label}"));
        fs::create_dir_all(&run_dir).map_err(io_err(format!("creating {}", run_dir.display())))?;
        Ok(Env {
            exe,
            profile: self.profile(),
            library: self.library(),
            run_dir,
            history: self.history_path(),
            pass_gap: self.0.options.pass_gap,
            verbose: self.settings().verbose,
            flasher_env: self.0.options.flasher_env.clone(),
        })
    }

    fn require_flasher(&self) -> Result<PathBuf> {
        let (exe, _) = self.flasher().ok_or_else(|| {
            CoreError::Refused(
                "VolvoFlasher.exe was not found. Choose its location in Connection.".into(),
            )
        })?;
        if !exe.is_file() {
            return Err(CoreError::Refused(format!(
                "VolvoFlasher.exe is missing at {}.",
                exe.display()
            )));
        }
        Ok(exe)
    }

    fn require_device(device: &str) -> Result<()> {
        if device.trim().is_empty() {
            return Err(CoreError::Refused(
                "Choose a J2534 device in Connection first.".into(),
            ));
        }
        Ok(())
    }

    pub fn preview_flash(&self, tune_path: &str, backup_id: &str) -> Result<FlashPreview> {
        let exe = self.require_flasher()?;
        let scratch = self.scratch_dir("probe")?;
        let library = self.library();
        let ctx = AssessCtx {
            exe: &exe,
            profile: self.profile(),
            library: &library,
            scratch: &scratch,
            env: &self.0.options.flasher_env,
        };
        Ok(assess(&ctx, tune_path, backup_id)?.preview)
    }

    pub fn start_backup(
        &self,
        sink: Arc<dyn EventSink>,
        device: String,
        verify: bool,
    ) -> Result<JobInfo> {
        Self::require_device(&device)?;
        let exe = self.require_flasher()?;
        if self.is_busy() {
            return Err(CoreError::Busy);
        }
        let env = self.make_env(exe, "backup")?;
        jobs::spawn(&self.0.shared, sink, env, JobKind::Backup, move |run| {
            run_backup(run, &device, verify)
        })
    }

    /// Re-checks every safety gate on the backend, whatever the UI claims, then starts the flash.
    pub fn start_flash(&self, sink: Arc<dyn EventSink>, req: FlashRequest) -> Result<JobInfo> {
        if self.is_busy() {
            return Err(CoreError::Busy);
        }
        if req.confirm.trim() != CONFIRM_PHRASE {
            return Err(CoreError::Refused(format!(
                "Type {CONFIRM_PHRASE} to confirm."
            )));
        }
        if PREFLIGHT_ITEMS
            .iter()
            .any(|item| !req.preflight.iter().any(|ticked| ticked == item))
        {
            return Err(CoreError::Refused(
                "Tick every item on the checklist first.".into(),
            ));
        }
        Self::require_device(&req.device)?;
        let exe = self.require_flasher()?;

        let scratch = self.scratch_dir("probe")?;
        let library = self.library();
        let ctx = AssessCtx {
            exe: &exe,
            profile: self.profile(),
            library: &library,
            scratch: &scratch,
            env: &self.0.options.flasher_env,
        };
        let assessment = assess(&ctx, &req.tune_path, &req.backup_id)?;
        if let Some(blocker) = assessment.preview.blockers.first() {
            return Err(CoreError::Refused(blocker.clone()));
        }
        if assessment.preview.requires_checksum_ack && !req.allow_checksum_changes {
            return Err(CoreError::Refused(
                "Confirm that the flasher may correct checksums, or flash a file that already passes.".into(),
            ));
        }

        let env = self.make_env(exe, "flash")?;
        jobs::spawn(&self.0.shared, sink, env, JobKind::Flash, move |run| {
            run_flash(run, req, assessment)
        })
    }

    /// Removes old flasher working directories. Best effort.
    fn prune_runs(&self) {
        let Ok(entries) = fs::read_dir(self.log_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age > RUN_DIR_MAX_AGE);
            if old && entry.path().is_dir() {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

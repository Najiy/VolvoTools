use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Result, io_err};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Explicit path to VolvoFlasher.exe. When unset the app looks next to itself and in the repo build tree.
    pub flasher_path: Option<String>,
    /// Where backups are stored. Defaults to Documents\VolvoTools\Backups.
    pub backups_dir: Option<String>,
    /// Last J2534 device the user picked.
    pub device: Option<String>,
    /// Read the ECU twice and compare, so a bad transfer cannot pass as a backup.
    pub verify_reads: bool,
    /// Read the written area back after a flash and compare it with the file.
    pub verify_after_flash: bool,
    /// Pass `-v` to the flasher so its library log shows in the activity view.
    pub verbose: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            flasher_path: None,
            backups_dir: None,
            device: None,
            verify_reads: true,
            verify_after_flash: true,
            verbose: false,
        }
    }
}

impl Settings {
    /// Trims text fields and turns blanks into `None`.
    pub fn normalized(mut self) -> Self {
        for field in [
            &mut self.flasher_path,
            &mut self.backups_dir,
            &mut self.device,
        ] {
            *field = field
                .take()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        self
    }
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// A missing or unreadable file yields the defaults; settings must never block startup.
    pub fn load(&self) -> Settings {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
            .unwrap_or_default()
            .normalized()
    }

    pub fn save(&self, settings: &Settings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io_err(format!("creating {}", parent.display())))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(settings).expect("settings always serialize");
        fs::write(&tmp, text).map_err(io_err(format!("writing {}", tmp.display())))?;
        fs::rename(&tmp, &self.path).map_err(io_err(format!("replacing {}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing_or_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(dir.path().join("settings.json"));
        assert_eq!(store.load(), Settings::default());
        fs::write(dir.path().join("settings.json"), "{ not json").unwrap();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn round_trips_and_normalizes() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(dir.path().join("nested").join("settings.json"));
        let settings = Settings {
            flasher_path: Some("  C:\\tools\\VolvoFlasher.exe ".into()),
            device: Some("   ".into()),
            verify_reads: false,
            ..Settings::default()
        };
        store.save(&settings).unwrap();
        let loaded = store.load();
        assert_eq!(
            loaded.flasher_path.as_deref(),
            Some("C:\\tools\\VolvoFlasher.exe")
        );
        assert_eq!(loaded.device, None);
        assert!(!loaded.verify_reads);
        assert!(loaded.verify_after_flash);
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"verifyReads":false,"somethingNew":1}"#).unwrap();
        let loaded = SettingsStore::new(path).load();
        assert!(!loaded.verify_reads);
        assert!(loaded.verify_after_flash);
    }
}

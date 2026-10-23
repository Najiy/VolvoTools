//! On-disk backup library: one folder per backup holding `ecu.bin` and `meta.json`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result, io_err};
use crate::hashing;

const IMAGE_FILE: &str = "ecu.bin";
const META_FILE: &str = "meta.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupMeta {
    /// Folder name inside the library.
    pub id: String,
    pub created_at: String,
    pub profile: String,
    pub size: u64,
    pub sha256: String,
    /// Two independent reads produced identical bytes.
    pub verified: bool,
    /// SHA-256 of each read, in order.
    pub reads: Vec<String>,
    pub device: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    #[serde(flatten)]
    pub meta: BackupMeta,
    /// Absolute path of the image.
    pub path: String,
    /// Absolute path of the backup folder.
    pub dir: String,
}

#[derive(Debug, Clone)]
pub struct NewEntry {
    pub id: String,
    pub dir: PathBuf,
}

impl NewEntry {
    pub fn image_path(&self) -> PathBuf {
        self.dir.join(IMAGE_FILE)
    }
}

#[derive(Debug, Clone)]
pub struct Library {
    root: PathBuf,
}

/// Ids come from the UI, so they must not be able to escape the library folder.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Library {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates a fresh, empty backup folder named after the current time.
    pub fn create_entry(&self, profile_id: &str) -> Result<NewEntry> {
        fs::create_dir_all(&self.root)
            .map_err(io_err(format!("creating {}", self.root.display())))?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let base = format!("{stamp}_{profile_id}");
        for attempt in 1..100 {
            let id = if attempt == 1 {
                base.clone()
            } else {
                format!("{base}-{attempt}")
            };
            let dir = self.root.join(&id);
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(NewEntry { id, dir }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_err(format!("creating {}", dir.display()))(e)),
            }
        }
        Err(CoreError::Invalid(
            "Could not find a free backup folder name".into(),
        ))
    }

    /// Removes a folder created by `create_entry` that never produced a usable image.
    pub fn discard(&self, entry: &NewEntry) {
        let _ = fs::remove_dir_all(&entry.dir);
    }

    pub fn write_meta(&self, entry: &NewEntry, meta: &BackupMeta) -> Result<()> {
        let path = entry.dir.join(META_FILE);
        let text = serde_json::to_string_pretty(meta).expect("metadata always serializes");
        fs::write(&path, text).map_err(io_err(format!("writing {}", path.display())))
    }

    pub fn image_path(&self, id: &str) -> PathBuf {
        self.root.join(id).join(IMAGE_FILE)
    }

    fn load_entry(&self, dir: &Path) -> Option<BackupEntry> {
        let text = fs::read_to_string(dir.join(META_FILE)).ok()?;
        let meta: BackupMeta = serde_json::from_str(&text).ok()?;
        if meta.id != dir.file_name()?.to_str()? {
            return None;
        }
        Some(BackupEntry {
            path: dir.join(IMAGE_FILE).display().to_string(),
            dir: dir.display().to_string(),
            meta,
        })
    }

    /// Newest first. Folders without readable metadata are skipped, not reported as errors.
    pub fn list(&self) -> Vec<BackupEntry> {
        let Ok(read_dir) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut entries: Vec<BackupEntry> = read_dir
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| self.load_entry(&e.path()))
            .collect();
        entries.sort_by(|a, b| b.meta.id.cmp(&a.meta.id));
        entries
    }

    pub fn get(&self, id: &str) -> Result<BackupEntry> {
        if !is_valid_id(id) {
            return Err(CoreError::Invalid(format!("Invalid backup id {id:?}")));
        }
        self.load_entry(&self.root.join(id))
            .ok_or_else(|| CoreError::Invalid(format!("Backup {id} was not found")))
    }

    /// Loads the image of a backup and re-checks it against the hash recorded when it was made.
    pub fn load_checked(&self, id: &str, expected_size: usize) -> Result<(BackupEntry, Vec<u8>)> {
        let entry = self.get(id)?;
        let path = self.image_path(id);
        let data = fs::read(&path).map_err(io_err(format!("reading {}", path.display())))?;
        if data.len() != expected_size {
            return Err(CoreError::Invalid(format!(
                "Backup image is {} bytes, expected {expected_size}",
                data.len()
            )));
        }
        if hashing::sha256_hex(&data) != entry.meta.sha256 {
            return Err(CoreError::Invalid(
                "The backup file no longer matches the checksum recorded when it was made".into(),
            ));
        }
        Ok((entry, data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, sha: &str) -> BackupMeta {
        BackupMeta {
            id: id.into(),
            created_at: "2026-10-23T10:00:00+00:00".into(),
            profile: "p1-me9".into(),
            size: 4,
            sha256: sha.into(),
            verified: true,
            reads: vec![sha.into(), sha.into()],
            device: "Test".into(),
            note: String::new(),
        }
    }

    #[test]
    fn create_write_list_get() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::new(dir.path().to_path_buf());
        let a = lib.create_entry("p1-me9").unwrap();
        fs::write(a.image_path(), [1, 2, 3, 4]).unwrap();
        lib.write_meta(&a, &meta(&a.id, &hashing::sha256_hex(&[1, 2, 3, 4])))
            .unwrap();

        let listed = lib.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].meta.id, a.id);
        assert!(listed[0].path.ends_with("ecu.bin"));
        assert_eq!(lib.get(&a.id).unwrap().meta.profile, "p1-me9");
    }

    #[test]
    fn ids_cannot_escape_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::new(dir.path().to_path_buf());
        for bad in ["", "..", "../x", "a/b", "a\\b", "C:\\x", "x y"] {
            assert!(lib.get(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn folders_without_metadata_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("stray")).unwrap();
        fs::write(dir.path().join("note.txt"), "x").unwrap();
        assert!(Library::new(dir.path().to_path_buf()).list().is_empty());
    }

    #[test]
    fn same_second_entries_get_distinct_ids() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::new(dir.path().to_path_buf());
        let a = lib.create_entry("p1-me9").unwrap();
        let b = lib.create_entry("p1-me9").unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn load_checked_detects_tampering_and_wrong_size() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::new(dir.path().to_path_buf());
        let e = lib.create_entry("p1-me9").unwrap();
        fs::write(e.image_path(), [1, 2, 3, 4]).unwrap();
        lib.write_meta(&e, &meta(&e.id, &hashing::sha256_hex(&[1, 2, 3, 4])))
            .unwrap();
        assert!(lib.load_checked(&e.id, 4).is_ok());
        assert!(lib.load_checked(&e.id, 8).is_err());

        fs::write(e.image_path(), [1, 2, 3, 5]).unwrap();
        let err = lib.load_checked(&e.id, 4).unwrap_err().to_string();
        assert!(err.contains("no longer matches"), "{err}");
    }

    #[test]
    fn metadata_for_another_folder_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::new(dir.path().to_path_buf());
        let e = lib.create_entry("p1-me9").unwrap();
        lib.write_meta(&e, &meta("some-other-id", "00")).unwrap();
        assert!(lib.list().is_empty());
    }
}

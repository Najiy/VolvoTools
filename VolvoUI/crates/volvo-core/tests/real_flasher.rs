//! Contract test of the Rust back end against the real VolvoFlasher.exe.
//!
//! It needs no car and no adapter: it uses `devices` and `flash --dry-run` only. Run it after
//! building VolvoFlasher with `--json` support:
//!
//! ```text
//! set VOLVOUI_FLASHER_EXE=C:\path\to\VolvoFlasher.exe
//! cargo test -p volvo-core --test real_flasher -- --ignored
//! ```

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use volvo_core::library::{BackupMeta, Library};
use volvo_core::settings::Settings;
use volvo_core::{ChecksumStatus, Core, CoreOptions, Dirs, hashing};

const SIZE: usize = 0x20_0000;
/// Mirrors `CheckBoundsME7` for 2 MiB images in Common/src/ChecksumHelper.cpp.
const TABLES: [(usize, usize); 2] = [(0xA_0000, 0xA_0360), (0x1C_91E0, 0x1C_9240)];

/// Deterministic pseudo-random bytes, so a failing run can be reproduced.
fn noise(seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..SIZE)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Noise plus a correct ME7-style checksum table: 60 records of (start, end, sum, !sum).
fn image_with_valid_table(seed: u64) -> Vec<u8> {
    let mut data = noise(seed);
    let mut n = 0;
    for (lo, hi) in TABLES {
        for at in (lo..hi).step_by(0x10) {
            let start = 0x10_0000 + n * 0x400;
            let end = start + 0x200;
            n += 1;
            let sum = (start..end).step_by(2).fold(0u32, |acc, a| {
                acc.wrapping_add(((data[a + 1] as u32) << 8) + data[a] as u32)
            });
            data[at..at + 4].copy_from_slice(&(start as u32).to_le_bytes());
            data[at + 4..at + 8].copy_from_slice(&(end as u32).to_le_bytes());
            data[at + 8..at + 12].copy_from_slice(&sum.to_le_bytes());
            data[at + 12..at + 16].copy_from_slice(&(!sum).to_le_bytes());
        }
    }
    data
}

struct Setup {
    dir: tempfile::TempDir,
    core: Core,
}

fn setup() -> Option<Setup> {
    let Some(exe) = std::env::var_os("VOLVOUI_FLASHER_EXE").map(PathBuf::from) else {
        eprintln!("VOLVOUI_FLASHER_EXE is not set; skipping");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let core = Core::new(
        Dirs {
            settings_file: dir.path().join("settings.json"),
            data_dir: dir.path().join("data"),
            default_backups_dir: dir.path().join("backups"),
        },
        None,
        CoreOptions {
            pass_gap: Duration::ZERO,
            flasher_override: Some(exe),
            flasher_env: Vec::new(),
        },
    );
    core.save_settings(Settings::default()).unwrap();
    Some(Setup { dir, core })
}

impl Setup {
    /// Stores `image` as a verified backup, as a finished two-read backup would.
    fn backup(&self, image: &[u8]) -> String {
        let library = Library::new(self.dir.path().join("backups"));
        let entry = library.create_entry("p1-me9").unwrap();
        fs::write(entry.image_path(), image).unwrap();
        let sha256 = hashing::sha256_hex(image);
        library
            .write_meta(
                &entry,
                &BackupMeta {
                    id: entry.id.clone(),
                    created_at: "2026-10-23T10:00:00+00:00".into(),
                    profile: "p1-me9".into(),
                    size: SIZE as u64,
                    sha256: sha256.clone(),
                    verified: true,
                    reads: vec![sha256.clone(), sha256],
                    device: "contract test".into(),
                    note: String::new(),
                },
            )
            .unwrap();
        entry.id
    }

    fn file(&self, name: &str, data: &[u8]) -> String {
        let path = self.dir.path().join(name);
        fs::write(&path, data).unwrap();
        path.display().to_string()
    }
}

#[test]
#[ignore = "needs VOLVOUI_FLASHER_EXE, see the module documentation"]
fn the_real_flasher_passes_the_handshake() {
    let Some(s) = setup() else { return };
    let scan = s.core.scan().unwrap();
    assert!(
        scan.flasher.found && scan.flasher.compatible,
        "{:?}",
        scan.flasher
    );
}

#[test]
#[ignore = "needs VOLVOUI_FLASHER_EXE, see the module documentation"]
fn a_file_with_a_valid_table_passes_without_changes() {
    let Some(s) = setup() else { return };
    let stock = image_with_valid_table(1);
    let id = s.backup(&stock);
    let mut tune = stock.clone();
    tune[0x18_0000] ^= 0xFF; // Block B, outside every checksummed range
    let preview = s
        .core
        .preview_flash(&s.file("tune.bin", &tune), &id)
        .unwrap();

    let tune_checksum = preview.tune_checksum.unwrap();
    assert_eq!(
        tune_checksum.status,
        ChecksumStatus::Valid,
        "{:?}",
        preview.blockers
    );
    assert_eq!(tune_checksum.checked_blocks, 60);
    assert_eq!(
        preview.backup_checksum.unwrap().status,
        ChecksumStatus::Valid
    );
    assert!(preview.can_flash, "{:?}", preview.blockers);
    assert!(!preview.requires_checksum_ack);
    assert_eq!(
        preview.plan.unwrap().chunks.len(),
        2,
        "the real plan matches the expected layout"
    );
}

#[test]
#[ignore = "needs VOLVOUI_FLASHER_EXE, see the module documentation"]
fn a_file_with_a_stale_checksum_asks_for_acknowledgement() {
    let Some(s) = setup() else { return };
    let stock = image_with_valid_table(2);
    let id = s.backup(&stock);
    let mut tune = stock.clone();
    tune[0x10_0010] ^= 0xFF; // inside a checksummed range, so a record no longer matches
    let preview = s
        .core
        .preview_flash(&s.file("tune.bin", &tune), &id)
        .unwrap();

    let verdict = preview.tune_checksum.unwrap();
    assert_eq!(verdict.status, ChecksumStatus::Correctable);
    assert!(verdict.modified_bytes > 0);
    assert!(preview.requires_checksum_ack);
    assert!(
        preview.can_flash,
        "the routine accepts the stock image: {:?}",
        preview.blockers
    );
}

#[test]
#[ignore = "needs VOLVOUI_FLASHER_EXE, see the module documentation"]
fn a_routine_that_cannot_read_the_stock_image_blocks_correction() {
    let Some(s) = setup() else { return };
    let stock = noise(3); // no table the routine understands
    let id = s.backup(&stock);
    let tune = image_with_valid_table(4);
    let mut broken = tune.clone();
    broken[0x10_0010] ^= 0xFF;
    let preview = s
        .core
        .preview_flash(&s.file("tune.bin", &broken), &id)
        .unwrap();

    assert_eq!(
        preview.backup_checksum.unwrap().status,
        ChecksumStatus::Unsupported
    );
    assert_eq!(
        preview.tune_checksum.unwrap().status,
        ChecksumStatus::Correctable
    );
    assert!(
        !preview.can_flash,
        "correction must not be trusted without a self-test"
    );
    assert!(
        preview.blockers.iter().any(|b| b.contains("stock")),
        "{:?}",
        preview.blockers
    );
}

#[test]
#[ignore = "needs VOLVOUI_FLASHER_EXE, see the module documentation"]
fn random_data_is_reported_as_unchecked_not_as_valid() {
    let Some(s) = setup() else { return };
    let stock = noise(5);
    let id = s.backup(&stock);
    let mut tune = stock.clone();
    tune[0x18_0000] ^= 0xFF;
    let preview = s
        .core
        .preview_flash(&s.file("tune.bin", &tune), &id)
        .unwrap();

    let verdict = preview.tune_checksum.unwrap();
    assert_eq!(
        verdict.status,
        ChecksumStatus::Unsupported,
        "an empty check is not a pass"
    );
    assert_eq!(verdict.checked_blocks, 0);
    assert!(
        preview
            .warnings
            .iter()
            .any(|w| w.contains("cannot check checksums"))
    );
}

#[test]
#[ignore = "needs VOLVOUI_FLASHER_EXE, see the module documentation"]
fn too_short_files_are_refused_before_the_flasher_is_asked() {
    let Some(s) = setup() else { return };
    let stock = noise(6);
    let id = s.backup(&stock);
    let preview = s
        .core
        .preview_flash(&s.file("short.bin", &stock[..1_000_000]), &id)
        .unwrap();
    assert!(!preview.can_flash && preview.diff.is_none());
}

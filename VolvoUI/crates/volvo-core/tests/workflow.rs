//! End-to-end tests of the backup and flash workflows against the simulated flasher.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tempfile::TempDir;
use volvo_core::events::{EventSink, JobEvent, JobInfo, JobOutcome};
use volvo_core::{
    CONFIRM_PHRASE, ChecksumStatus, Core, CoreError, CoreOptions, Dirs, FlashRequest,
    PREFLIGHT_ITEMS, hashing,
};

const FAKE: &str = env!("CARGO_BIN_EXE_fake_flasher");
const DEVICE: &str = "Simulated J2534 device";
const SIZE: usize = 0x20_0000;

struct Sink(Mutex<mpsc::Sender<JobEvent>>);

impl EventSink for Sink {
    fn emit(&self, event: &JobEvent) {
        let _ = self.0.lock().unwrap().send(event.clone());
    }
}

struct Harness {
    dir: TempDir,
    core: Core,
}

impl Harness {
    fn new(fake_env: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut env = vec![(
            "FAKE_ECU_IMAGE".to_string(),
            dir.path().join("ecu.bin").display().to_string(),
        )];
        env.extend(fake_env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let core = Core::new(
            Dirs {
                settings_file: dir.path().join("settings.json"),
                data_dir: dir.path().join("data"),
                default_backups_dir: dir.path().join("backups"),
            },
            None,
            CoreOptions {
                pass_gap: Duration::ZERO,
                flasher_override: Some(PathBuf::from(FAKE)),
                flasher_env: env,
            },
        );
        Harness { dir, core }
    }

    fn ecu_image(&self) -> Vec<u8> {
        fs::read(self.dir.path().join("ecu.bin")).unwrap()
    }

    fn try_start(
        &self,
        start: impl FnOnce(&Core, Arc<dyn EventSink>) -> Result<JobInfo, CoreError>,
    ) -> Result<mpsc::Receiver<JobEvent>, CoreError> {
        let (tx, rx) = mpsc::channel();
        start(&self.core, Arc::new(Sink(Mutex::new(tx))))?;
        Ok(rx)
    }

    fn start_backup(&self, verify: bool) -> mpsc::Receiver<JobEvent> {
        self.try_start(|core, sink| core.start_backup(sink, DEVICE.into(), verify))
            .unwrap()
    }

    fn backup(&self, verify: bool) -> (JobOutcome, Vec<JobEvent>) {
        finish(self.start_backup(verify))
    }

    fn verified_backup_id(&self) -> String {
        let (outcome, _) = self.backup(true);
        assert!(outcome.ok, "{}", outcome.message);
        outcome.backup.unwrap().meta.id
    }

    fn tune(&self, edit: impl FnOnce(&mut Vec<u8>)) -> (PathBuf, Vec<u8>) {
        let mut data = self.ecu_image();
        edit(&mut data);
        let path = self.dir.path().join("tune.bin");
        fs::write(&path, &data).unwrap();
        (path, data)
    }

    fn request(&self, tune: &Path, backup_id: &str) -> FlashRequest {
        FlashRequest {
            device: DEVICE.into(),
            tune_path: tune.display().to_string(),
            backup_id: backup_id.into(),
            confirm: CONFIRM_PHRASE.into(),
            preflight: PREFLIGHT_ITEMS.iter().map(|s| s.to_string()).collect(),
            verify_after: true,
            allow_checksum_changes: false,
        }
    }

    fn try_flash(&self, req: FlashRequest) -> Result<mpsc::Receiver<JobEvent>, CoreError> {
        self.try_start(|core, sink| core.start_flash(sink, req))
    }

    fn flash(&self, req: FlashRequest) -> (JobOutcome, Vec<JobEvent>) {
        finish(self.try_flash(req).unwrap())
    }
}

fn finish(rx: mpsc::Receiver<JobEvent>) -> (JobOutcome, Vec<JobEvent>) {
    let mut events = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(JobEvent::Finished { outcome }) => return (outcome, events),
            Ok(event) => events.push(event),
            Err(e) => panic!(
                "the job did not finish ({e}); last events: {:?}",
                events.iter().rev().take(4).collect::<Vec<_>>()
            ),
        }
    }
}

/// Consumes events up to and including the first one matching `matches`.
fn wait_for(rx: &mpsc::Receiver<JobEvent>, matches: impl Fn(&JobEvent) -> bool) {
    loop {
        let event = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the expected event never arrived");
        if matches(&event) {
            return;
        }
    }
}

fn phases(events: &[JobEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            JobEvent::Phase(p) => Some(p.label.clone()),
            _ => None,
        })
        .collect()
}

fn refused(result: Result<mpsc::Receiver<JobEvent>, CoreError>) -> String {
    match result {
        Err(CoreError::Refused(message)) => message,
        Err(other) => panic!("expected a refusal, got {other:?}"),
        Ok(_) => panic!("expected a refusal, but the job started"),
    }
}

// ---------------------------------------------------------------- connection

#[test]
fn scan_accepts_the_simulator_and_lists_its_device() {
    let h = Harness::new(&[]);
    let scan = h.core.scan().unwrap();
    assert!(
        scan.flasher.found && scan.flasher.compatible,
        "{:?}",
        scan.flasher
    );
    assert_eq!(scan.devices.len(), 1);
    assert_eq!(scan.devices[0].name, DEVICE);
}

#[test]
fn scan_rejects_a_flasher_with_another_protocol_version() {
    let h = Harness::new(&[("FAKE_PROTOCOL", "2")]);
    let scan = h.core.scan().unwrap();
    assert!(scan.flasher.found && !scan.flasher.compatible);
    assert!(
        scan.flasher.error.unwrap().contains("protocol 2"),
        "the message should name the mismatch"
    );
}

#[test]
fn scan_without_a_flasher_explains_what_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::new(
        Dirs {
            settings_file: dir.path().join("settings.json"),
            data_dir: dir.path().join("data"),
            default_backups_dir: dir.path().join("backups"),
        },
        Some(dir.path().to_path_buf()),
        CoreOptions::default(),
    );
    let scan = core.scan().unwrap();
    assert!(!scan.flasher.found);
    assert!(scan.flasher.error.unwrap().contains("VolvoFlasher.exe"));
}

#[test]
fn no_device_means_no_job() {
    let h = Harness::new(&[]);
    let blank = h.try_start(|core, sink| core.start_backup(sink, "  ".into(), true));
    assert!(refused(blank).contains("device"));
    assert!(!h.core.is_busy());
}

// -------------------------------------------------------------------- backup

#[test]
fn backup_reads_twice_and_stores_a_verified_copy() {
    let h = Harness::new(&[]);
    let (outcome, events) = h.backup(true);
    assert!(outcome.ok, "{}", outcome.message);

    let backup = outcome.backup.expect("the outcome carries the backup");
    assert!(backup.meta.verified);
    assert_eq!(backup.meta.reads.len(), 2);
    assert_eq!(backup.meta.size, SIZE as u64);
    let stored = fs::read(&backup.path).unwrap();
    assert_eq!(stored, h.ecu_image());
    assert_eq!(hashing::sha256_hex(&stored), backup.meta.sha256);
    assert!(!Path::new(&backup.dir).join("read2.bin").exists());
    assert_eq!(h.core.backups().len(), 1);

    assert_eq!(phases(&events), ["Read 1 of 2", "Read 2 of 2"]);
    let states: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            JobEvent::State { state } => Some(state.as_str()),
            _ => None,
        })
        .collect();
    assert!(states.contains(&"LoadBootloader") && states.contains(&"Done"));
    assert!(
        states.contains(&"WriteFlash"),
        "the real flasher reports a read as WriteFlash too"
    );
}

#[test]
fn progress_never_goes_backwards_within_a_phase() {
    let h = Harness::new(&[]);
    let (_, events) = h.backup(true);
    let mut last = 0;
    let mut seen = 0;
    for event in &events {
        match event {
            JobEvent::Phase(_) => last = 0,
            JobEvent::Progress(p) => {
                assert!(
                    p.current >= last && p.current <= p.max,
                    "{p:?} after {last}"
                );
                last = p.current;
                seen += 1;
            }
            _ => {}
        }
    }
    assert!(seen > 20, "expected a stream of progress, got {seen}");
}

#[test]
fn the_log_shows_the_command_that_was_run() {
    let h = Harness::new(&[]);
    let (_, events) = h.backup(false);
    let command = events.iter().find_map(|e| match e {
        JobEvent::Log(l) if l.line.starts_with("> ") => Some(l.line.clone()),
        _ => None,
    });
    let command = command.expect("a command line is logged");
    assert!(command.contains("read") && command.contains("--cm-info me9_p1"));
    assert!(
        command.contains("-d \"Simulated J2534 device\""),
        "{command}"
    );
}

#[test]
fn a_corrupted_second_read_leaves_an_unverified_backup() {
    let h = Harness::new(&[("FAKE_CORRUPT_READ", "2")]);
    let (outcome, _) = h.backup(true);
    assert!(!outcome.ok);
    assert!(outcome.message.contains("differ"), "{}", outcome.message);
    let backup = outcome.backup.expect("kept for inspection");
    assert!(!backup.meta.verified);
    assert_ne!(backup.meta.reads[0], backup.meta.reads[1]);
    assert!(Path::new(&backup.dir).join("read2.bin").exists());
}

#[test]
fn a_single_read_is_saved_but_not_verified() {
    let h = Harness::new(&[]);
    let (outcome, events) = h.backup(false);
    assert!(outcome.ok);
    assert!(!outcome.backup.unwrap().meta.verified);
    assert_eq!(phases(&events), ["Read 1 of 1"]);
}

#[test]
fn a_failed_first_read_leaves_nothing_behind() {
    let h = Harness::new(&[("FAKE_FAIL", "read")]);
    let (outcome, _) = h.backup(true);
    assert!(!outcome.ok && outcome.backup.is_none());
    assert!(outcome.message.contains("Simulated read failure"));
    assert!(h.core.backups().is_empty());
    let leftovers = fs::read_dir(h.dir.path().join("backups"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "the empty backup folder is removed");
}

#[test]
fn stopping_a_backup_discards_it() {
    let h = Harness::new(&[("FAKE_DELAY_MS", "15")]);
    let rx = h.start_backup(true);
    wait_for(&rx, |e| matches!(e, JobEvent::Progress(_)));
    h.core.cancel(false).expect("a read can always be stopped");
    let (outcome, _) = finish(rx);
    assert!(outcome.cancelled && !outcome.ok);
    assert!(
        outcome.hint.is_some_and(|hint| hint.contains("ignition")),
        "a stopped read should tell the user to power-cycle the ECU"
    );
    assert!(h.core.backups().is_empty());
    assert!(!h.core.is_busy());
}

#[test]
fn only_one_job_runs_at_a_time() {
    let h = Harness::new(&[("FAKE_DELAY_MS", "15")]);
    let rx = h.start_backup(true);
    let second = h.try_start(|core, sink| core.start_backup(sink, DEVICE.into(), true));
    assert!(matches!(second, Err(CoreError::Busy)));
    assert!(h.core.snapshot().running);
    h.core.cancel(false).unwrap();
    finish(rx);
    let snapshot = h.core.snapshot();
    assert!(!snapshot.running && snapshot.outcome.is_some());
}

#[test]
fn finished_jobs_are_recorded_in_the_history() {
    let h = Harness::new(&[]);
    h.backup(true);
    let history = h.core.history(10);
    assert_eq!(history.len(), 1);
    assert!(history[0].ok && history[0].detail.contains("sha256"));
}

// --------------------------------------------------------------------- flash

#[test]
fn preview_reports_the_diff_and_a_clean_plan() {
    let h = Harness::new(&[]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| {
        d[0x3_0000] ^= 0xFF;
        d[0xB_0000] ^= 0xFF;
        d[0xB_0001] ^= 0xFF;
    });
    let preview = h
        .core
        .preview_flash(&tune.display().to_string(), &id)
        .unwrap();
    assert!(preview.can_flash, "{:?}", preview.blockers);
    assert!(preview.warnings.is_empty(), "{:?}", preview.warnings);
    let diff = preview.diff.unwrap();
    assert_eq!((diff.total_changed, diff.changed_in_written), (3, 3));
    let tune_checksum = preview.tune_checksum.unwrap();
    assert_eq!(tune_checksum.status, ChecksumStatus::Valid);
    assert_eq!(tune_checksum.checked_blocks, 60);
    assert_eq!(
        preview.backup_checksum.unwrap().status,
        ChecksumStatus::Valid
    );
    assert_eq!(preview.plan.unwrap().chunks.len(), 2);
}

#[test]
fn flash_writes_the_file_and_verifies_it() {
    let h = Harness::new(&[]);
    let id = h.verified_backup_id();
    let (tune, data) = h.tune(|d| {
        d[0x3_0000] ^= 0xFF;
        d[0xB_0000] ^= 0xFF;
    });
    let (outcome, events) = h.flash(h.request(&tune, &id));
    assert!(outcome.ok, "{}", outcome.message);
    assert!(outcome.message.contains("verified"));
    let verify = outcome.verify.unwrap();
    assert!(verify.written_ok && !verify.untouched_changed);
    assert_eq!(phases(&events), ["Flash", "Verify"]);

    let image = h.ecu_image();
    assert_eq!(image[0x2_0000..0x9_0000], data[0x2_0000..0x9_0000]);
    assert_eq!(image[0xA_0000..0x1F_0000], data[0xA_0000..0x1F_0000]);
    let history = h.core.history(10);
    assert_eq!(history.len(), 2, "backup and flash");
    assert!(history[0].detail.contains("tune.bin"));
}

#[test]
fn changes_outside_the_written_area_are_flagged_and_not_applied() {
    let h = Harness::new(&[]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| {
        d[0x10] ^= 0xFF; // low block, never written
        d[0xB_0000] ^= 0xFF;
    });
    let preview = h
        .core
        .preview_flash(&tune.display().to_string(), &id)
        .unwrap();
    assert!(preview.can_flash);
    assert!(preview.warnings.iter().any(|w| w.contains("never writes")));

    let before = h.ecu_image();
    let (outcome, _) = h.flash(h.request(&tune, &id));
    assert!(outcome.ok, "{}", outcome.message);
    assert_eq!(
        h.ecu_image()[..0x2_0000],
        before[..0x2_0000],
        "the low block is untouched"
    );
}

#[test]
fn flash_needs_the_phrase_and_every_checklist_item() {
    let h = Harness::new(&[]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);

    let mut req = h.request(&tune, &id);
    req.confirm = "flash".into();
    assert!(refused(h.try_flash(req)).contains(CONFIRM_PHRASE));

    let mut req = h.request(&tune, &id);
    req.preflight.pop();
    assert!(refused(h.try_flash(req)).contains("checklist"));

    let mut req = h.request(&tune, &id);
    req.device = String::new();
    assert!(refused(h.try_flash(req)).contains("device"));
    assert!(!h.core.is_busy());
}

#[test]
fn flash_refuses_a_backup_without_two_matching_reads() {
    let h = Harness::new(&[]);
    let (outcome, _) = h.backup(false);
    let id = outcome.backup.unwrap().meta.id;
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let message = refused(h.try_flash(h.request(&tune, &id)));
    assert!(message.contains("two matching reads"), "{message}");
}

#[test]
fn flash_refuses_a_backup_that_was_modified_afterwards() {
    let h = Harness::new(&[]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let mut stored = fs::read(h.core.library().image_path(&id)).unwrap();
    stored[5] ^= 1;
    fs::write(h.core.library().image_path(&id), stored).unwrap();
    let message = refused(h.try_flash(h.request(&tune, &id)));
    assert!(message.contains("no longer matches"), "{message}");
}

#[test]
fn flash_refuses_files_of_the_wrong_size() {
    let h = Harness::new(&[]);
    let id = h.verified_backup_id();
    let path = h.dir.path().join("short.bin");
    fs::write(&path, vec![0u8; 1_000_000]).unwrap();
    let preview = h
        .core
        .preview_flash(&path.display().to_string(), &id)
        .unwrap();
    assert!(!preview.can_flash && preview.diff.is_none());
    assert!(preview.blockers[0].contains("1000000 bytes"));
    assert!(refused(h.try_flash(h.request(&path, &id))).contains("bytes"));
}

#[test]
fn flash_refuses_a_file_identical_to_the_backup() {
    let h = Harness::new(&[]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|_| {});
    let message = refused(h.try_flash(h.request(&tune, &id)));
    assert!(message.contains("identical"), "{message}");
}

#[test]
fn a_checksum_correction_needs_an_explicit_acknowledgement() {
    let h = Harness::new(&[("FAKE_CHECKSUM", "correct")]);
    let id = h.verified_backup_id();
    let (tune, data) = h.tune(|d| d[0xB_0100] ^= 0xFF);
    let preview = h
        .core
        .preview_flash(&tune.display().to_string(), &id)
        .unwrap();
    assert!(preview.can_flash, "{:?}", preview.blockers);
    assert!(preview.requires_checksum_ack);
    let verdict = preview.tune_checksum.unwrap();
    assert_eq!(
        (verdict.status, verdict.modified_bytes),
        (ChecksumStatus::Correctable, 4)
    );
    assert_eq!(
        preview.backup_checksum.unwrap().status,
        ChecksumStatus::Valid,
        "the routine accepts the stock image"
    );

    let message = refused(h.try_flash(h.request(&tune, &id)));
    assert!(message.contains("correct checksums"), "{message}");

    let mut req = h.request(&tune, &id);
    req.allow_checksum_changes = true;
    let (outcome, _) = h.flash(req);
    assert!(outcome.ok, "{}", outcome.message);
    assert!(
        outcome.verify.unwrap().written_ok,
        "the read-back is compared with the corrected image, not the raw file"
    );
    let flashed = &h.ecu_image()[0xA_0000..0x1F_0000];
    let different = flashed
        .iter()
        .zip(&data[0xA_0000..0x1F_0000])
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        different, 4,
        "exactly the corrected bytes differ from the file"
    );
}

#[test]
fn a_checksum_routine_that_rejects_the_stock_image_is_not_trusted() {
    let h = Harness::new(&[("FAKE_CHECKSUM", "reject-stock")]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let preview = h
        .core
        .preview_flash(&tune.display().to_string(), &id)
        .unwrap();
    assert!(!preview.can_flash);
    assert!(preview.blockers.iter().any(|b| b.contains("stock")));
    let mut req = h.request(&tune, &id);
    req.allow_checksum_changes = true;
    assert!(
        refused(h.try_flash(req)).contains("stock"),
        "an acknowledgement cannot override this"
    );
}

#[test]
fn a_check_that_evaluated_nothing_is_not_reported_as_valid() {
    // The ME7-style check passes when its table is not in the expected format. That must not
    // count as "checksums are valid", and it must not count as a self-test of the routine either.
    let h = Harness::new(&[("FAKE_CHECKSUM", "blind")]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let preview = h
        .core
        .preview_flash(&tune.display().to_string(), &id)
        .unwrap();
    let tune_checksum = preview.tune_checksum.unwrap();
    assert_eq!(tune_checksum.status, ChecksumStatus::Unsupported);
    assert_eq!(tune_checksum.checked_blocks, 0);
    assert_eq!(
        preview.backup_checksum.unwrap().status,
        ChecksumStatus::Unsupported,
        "a routine that checked nothing has not been tested on the stock image"
    );
    assert!(preview.can_flash && !preview.requires_checksum_ack);
    assert!(
        preview
            .warnings
            .iter()
            .any(|w| w.contains("cannot check checksums"))
    );
}

#[test]
fn files_without_a_known_checksum_scheme_only_get_a_warning() {
    let h = Harness::new(&[("FAKE_CHECKSUM", "unsupported")]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let preview = h
        .core
        .preview_flash(&tune.display().to_string(), &id)
        .unwrap();
    assert!(preview.can_flash && !preview.requires_checksum_ack);
    assert!(
        preview
            .warnings
            .iter()
            .any(|w| w.contains("cannot check checksums"))
    );
}

#[test]
fn a_failure_before_the_erase_says_nothing_was_written() {
    let h = Harness::new(&[("FAKE_FAIL", "flash")]);
    let id = h.verified_backup_id();
    let before = h.ecu_image();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let (outcome, _) = h.flash(h.request(&tune, &id));
    assert!(!outcome.ok && !outcome.cancelled);
    assert!(outcome.hint.unwrap().contains("Nothing was written"));
    assert_eq!(h.ecu_image(), before);
}

#[test]
fn a_failure_after_the_erase_gives_recovery_advice() {
    let h = Harness::new(&[("FAKE_FAIL", "erase")]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let (outcome, _) = h.flash(h.request(&tune, &id));
    assert!(!outcome.ok);
    assert!(outcome.message.contains("Simulated erase failure"));
    let hint = outcome.hint.unwrap();
    assert!(
        hint.contains("erase had started") && hint.contains("again"),
        "{hint}"
    );
}

#[test]
fn a_flash_that_reports_success_but_wrote_a_wrong_byte_is_caught() {
    let h = Harness::new(&[("FAKE_BAD_WRITE", "1")]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xA_0000] ^= 0xFF);
    let (outcome, _) = h.flash(h.request(&tune, &id));
    assert!(!outcome.ok, "flasher success is not enough");
    assert!(
        outcome.message.contains("does not match"),
        "{}",
        outcome.message
    );
    let verify = outcome.verify.unwrap();
    assert!(!verify.written_ok);
    let block_b = verify.checks.iter().find(|c| c.name == "Block B").unwrap();
    assert_eq!(
        (block_b.mismatched, block_b.first_diff),
        (1, Some(0xB_0000))
    );
}

#[test]
fn an_erase_that_also_wipes_the_gap_is_reported_but_not_a_failure() {
    let h = Harness::new(&[("FAKE_ERASE_GAP", "1")]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let (outcome, _) = h.flash(h.request(&tune, &id));
    assert!(outcome.ok, "{}", outcome.message);
    assert!(outcome.verify.unwrap().untouched_changed);
    assert!(outcome.hint.unwrap().contains("gap"));
}

#[test]
fn verification_can_be_skipped_but_the_result_says_so() {
    let h = Harness::new(&[]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let mut req = h.request(&tune, &id);
    req.verify_after = false;
    let (outcome, events) = h.flash(req);
    assert!(outcome.ok && outcome.verify.is_none());
    assert!(outcome.message.contains("not verified"));
    assert_eq!(phases(&events), ["Flash"]);
}

#[test]
fn a_plain_stop_is_refused_while_writing_but_force_stop_works() {
    let h = Harness::new(&[("FAKE_DELAY_MS", "10")]);
    let id = h.verified_backup_id();
    let (tune, _) = h.tune(|d| d[0xB_0000] ^= 0xFF);
    let rx = h.try_flash(h.request(&tune, &id)).unwrap();

    wait_for(
        &rx,
        |e| matches!(e, JobEvent::State { state } if state == "WriteFlash"),
    );
    let err = h.core.cancel(false).unwrap_err();
    assert!(matches!(err, CoreError::Refused(_)), "{err:?}");
    assert!(h.core.is_busy(), "the refusal must not stop the job");

    h.core.cancel(true).unwrap();
    let (outcome, _) = finish(rx);
    assert!(outcome.cancelled);
    assert!(outcome.hint.unwrap().contains("erase had started"));
}

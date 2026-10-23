//! Flash workflow: assess a file against a verified backup, then flash it and read it back.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Result, io_err};
use crate::events::{JobOutcome, PhaseMode};
use crate::flasher::Cli;
use crate::hashing;
use crate::jobs::Run;
use crate::library::Library;
use crate::profile::{self, DiffReport, Expectation, Profile};
use crate::protocol::{Message, PROTOCOL_VERSION, Plan};
use crate::runner::{self, CancelToken, Flow, Invocation, Output};

pub const CONFIRM_PHRASE: &str = "FLASH";
/// Every item the user must tick before a flash can start.
pub const PREFLIGHT_ITEMS: [&str; 5] = ["charger", "ignition", "loads", "laptop", "cable"];

const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(30);
/// Share of the flashable area that may differ before the user is asked to double check the file.
const LARGE_CHANGE_SHARE: f64 = 0.25;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashRequest {
    pub device: String,
    pub tune_path: String,
    pub backup_id: String,
    pub confirm: String,
    pub preflight: Vec<String>,
    pub verify_after: bool,
    pub allow_checksum_changes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChecksumStatus {
    /// The file already satisfies the flasher's checksum scheme.
    Valid,
    /// The flasher would change bytes to make the file satisfy it.
    Correctable,
    /// The flasher has no checksum scheme for this file, or found no table in it that the
    /// scheme understands. Nothing was checked.
    Unsupported,
    /// The flasher could not evaluate the file.
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksumVerdict {
    pub status: ChecksumStatus,
    pub modified_bytes: u64,
    /// Checksum records the flasher actually evaluated in this file.
    pub checked_blocks: u64,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashPreview {
    pub file: FileInfo,
    pub backup_id: String,
    pub diff: Option<DiffReport>,
    pub plan: Option<Plan>,
    pub tune_checksum: Option<ChecksumVerdict>,
    /// The flasher's verdict on the stock backup: a self-test of its checksum routine.
    pub backup_checksum: Option<ChecksumVerdict>,
    /// Reasons the flash cannot start.
    pub blockers: Vec<String>,
    /// Things the user should know but may proceed past.
    pub warnings: Vec<String>,
    pub requires_checksum_ack: bool,
    pub can_flash: bool,
}

pub(crate) struct AssessCtx<'a> {
    pub exe: &'a Path,
    pub profile: &'static Profile,
    pub library: &'a Library,
    /// Working directory for the dry runs.
    pub scratch: &'a Path,
    /// Extra environment variables for the flasher process.
    pub env: &'a [(String, String)],
}

pub(crate) struct Assessment {
    pub preview: FlashPreview,
    pub tune: Vec<u8>,
    pub backup: Option<Vec<u8>>,
    /// The image the flasher will actually send: the file plus any checksum corrections.
    pub prepared: Option<Vec<u8>>,
}

fn verdict(plan: &Plan) -> ChecksumVerdict {
    let modified_bytes: u64 = plan.chunks.iter().map(|c| c.modified).sum();
    let checked_blocks = plan.checksum.checked_blocks;
    // `valid` alone is not evidence: the ME7-style check also passes when it found no table to
    // evaluate. Only a check that looked at something counts.
    let status = match (
        plan.checksum.supported && checked_blocks > 0,
        plan.checksum.valid,
    ) {
        (false, _) => ChecksumStatus::Unsupported,
        (true, true) if modified_bytes == 0 => ChecksumStatus::Valid,
        (true, _) => ChecksumStatus::Correctable,
    };
    ChecksumVerdict {
        status,
        modified_bytes,
        checked_blocks,
        detail: None,
    }
}

fn unavailable(detail: String) -> ChecksumVerdict {
    ChecksumVerdict {
        status: ChecksumStatus::Unavailable,
        modified_bytes: 0,
        checked_blocks: 0,
        detail: Some(detail),
    }
}

/// Asks the flasher what it would do with `file`, without touching any hardware.
/// With `want_image` the prepared image is returned as well.
fn dry_run(
    ctx: &AssessCtx,
    file: &Path,
    want_image: bool,
) -> std::result::Result<(Plan, Option<Vec<u8>>), String> {
    let cli = Cli {
        profile: ctx.profile,
        device: "",
        verbose: false,
    };
    let image_path = want_image.then(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        ctx.scratch
            .join(format!("prepared-{}-{nanos}.bin", std::process::id()))
    });
    let inv = Invocation {
        exe: ctx.exe.to_path_buf(),
        args: cli.dry_run(file, image_path.as_deref()),
        cwd: ctx.scratch.to_path_buf(),
        env: ctx.env.to_vec(),
    };
    let mut plan = None;
    let mut result: Option<(bool, String)> = None;
    let mut protocol_seen: Option<u32> = None;
    let mut first_raw: Option<String> = None;
    let outcome = runner::run(
        &inv,
        &CancelToken::default(),
        Some(DRY_RUN_TIMEOUT),
        |output| {
            match output {
                Output::Message(Message::Hello { protocol, .. }) => protocol_seen = Some(protocol),
                Output::Message(Message::Plan(p)) => plan = Some(p),
                Output::Message(Message::Result { ok, message }) => result = Some((ok, message)),
                Output::Raw { line, .. } => {
                    first_raw.get_or_insert(line);
                }
                _ => {}
            }
            Flow::Continue
        },
    )
    .map_err(|e| e.to_string())?;

    if outcome.timed_out {
        return Err("the flasher did not answer within 30 seconds".into());
    }
    match (protocol_seen, &result) {
        // A flasher that rejects its command line explains itself before it ever says hello.
        (None, Some((false, message))) => {
            return Err(match &first_raw {
                Some(detail) => format!("{message}: {detail}"),
                None => message.clone(),
            });
        }
        (None, _) => {
            return Err(
                "this VolvoFlasher build does not support the UI protocol, rebuild it".into(),
            );
        }
        (Some(p), _) if p != PROTOCOL_VERSION => {
            return Err(format!("protocol {p} is not supported"));
        }
        (Some(_), _) => {}
    }
    let plan = match (plan, result) {
        (Some(plan), Some((true, _))) => plan,
        (_, Some((false, message))) => return Err(message),
        _ => return Err("the flasher returned no plan".into()),
    };
    let image = match image_path {
        Some(path) => {
            let bytes = fs::read(&path)
                .map_err(|e| format!("the flasher did not write the prepared image: {e}"));
            let _ = fs::remove_file(&path);
            Some(bytes?)
        }
        None => None,
    };
    Ok((plan, image))
}

/// Collects everything known about flashing `tune_path` on top of `backup_id`.
/// Problems are reported as blockers and warnings; only unreadable files are errors.
pub(crate) fn assess(ctx: &AssessCtx, tune_path: &str, backup_id: &str) -> Result<Assessment> {
    let profile = ctx.profile;
    let path = PathBuf::from(tune_path);
    let tune = fs::read(&path).map_err(io_err(format!("reading {}", path.display())))?;

    let mut blockers: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut preview = FlashPreview {
        file: FileInfo {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.display().to_string(),
            size: tune.len() as u64,
            sha256: hashing::sha256_hex(&tune),
        },
        backup_id: backup_id.to_string(),
        diff: None,
        plan: None,
        tune_checksum: None,
        backup_checksum: None,
        blockers: Vec::new(),
        warnings: Vec::new(),
        requires_checksum_ack: false,
        can_flash: false,
    };

    if tune.len() != profile.image_size {
        blockers.push(format!(
            "The file is {} bytes. A full image for this ECU is {} bytes.",
            tune.len(),
            profile.image_size
        ));
        preview.blockers = blockers;
        return Ok(Assessment {
            preview,
            tune,
            backup: None,
            prepared: None,
        });
    }

    let backup = if backup_id.is_empty() {
        blockers.push("Choose a backup to compare against.".into());
        None
    } else {
        match ctx.library.load_checked(backup_id, profile.image_size) {
            Ok((entry, data)) => {
                if !entry.meta.verified {
                    blockers.push(
                        "The selected backup was not confirmed by two matching reads.".into(),
                    );
                }
                Some(data)
            }
            Err(e) => {
                blockers.push(e.to_string());
                None
            }
        }
    };

    if let Some(backup) = &backup {
        let diff = profile::diff(profile, backup, &tune)?;
        if diff.identical {
            blockers
                .push("The file is identical to the backup, so there is nothing to flash.".into());
        }
        if diff.changed_in_not_written > 0 {
            let names: Vec<&str> = diff
                .regions
                .iter()
                .filter(|r| r.role == profile::RegionRole::NotWritten && r.changed > 0)
                .map(|r| r.name)
                .collect();
            let count = diff.changed_in_not_written;
            warnings.push(if count == 1 {
                format!(
                    "1 changed byte lies in an area the flasher never writes ({}). That change will not be applied.",
                    names.join(", ")
                )
            } else {
                format!(
                    "{count} changed bytes lie in areas the flasher never writes ({}). Those changes will not be applied.",
                    names.join(", ")
                )
            });
        }
        let share = diff.changed_in_written as f64 / diff.written_bytes as f64;
        if share > LARGE_CHANGE_SHARE {
            warnings.push(format!(
                "{:.0}% of the flashable area differs from your backup. Check that this is a tune of the same software.",
                share * 100.0
            ));
        }
        preview.diff = Some(diff);
    }

    let mut prepared = None;
    match dry_run(ctx, &path, true) {
        Ok((plan, image)) => {
            let problems = profile.plan_problems(&plan);
            if !problems.is_empty() {
                blockers.push(format!(
                    "The flasher's write plan is not the expected one: {}.",
                    problems.join("; ")
                ));
            }
            preview.tune_checksum = Some(verdict(&plan));
            preview.plan = Some(plan);
            prepared = image.filter(|image| image.len() == profile.image_size);
            if prepared.is_none() {
                blockers.push("The flasher's prepared image has an unexpected size.".into());
            }
        }
        Err(message) => {
            blockers.push(format!(
                "The flasher could not prepare this file: {message}."
            ));
            preview.tune_checksum = Some(unavailable(message));
        }
    }
    if backup.is_some() {
        preview.backup_checksum = Some(
            match dry_run(ctx, &ctx.library.image_path(backup_id), false) {
                Ok((plan, _)) => verdict(&plan),
                Err(message) => unavailable(message),
            },
        );
    }

    match preview.tune_checksum.as_ref().map(|v| (v.status, v.modified_bytes)) {
        Some((ChecksumStatus::Correctable, modified)) => {
            preview.requires_checksum_ack = true;
            warnings.push(format!(
                "The flasher will change {} in this file to correct its checksums.",
                crate::text::bytes(modified)
            ));
            let routine_trusted =
                preview.backup_checksum.as_ref().is_some_and(|v| v.status == ChecksumStatus::Valid);
            if !routine_trusted {
                blockers.push(
                    "The flasher would correct checksums, but its checksum routine does not accept your own stock \
                     backup, so it cannot be trusted on this ECU. Correct the checksums in your tuning software and \
                     flash a file that already passes."
                        .into(),
                );
            }
        }
        Some((ChecksumStatus::Unsupported, _)) => warnings.push(
            "The flasher cannot check checksums for this file. Make sure your tuning software corrected them.".into(),
        ),
        _ => {}
    }

    preview.can_flash = blockers.is_empty();
    preview.blockers = blockers;
    preview.warnings = warnings;
    Ok(Assessment {
        preview,
        tune,
        backup,
        prepared,
    })
}

fn recovery_hint(erase_started: bool) -> Option<String> {
    Some(if erase_started {
        "The erase had started, so the ECU may not run until a complete flash succeeds. Keep the battery connected \
         and the ignition on, then run the same flash again. This tool never writes below 0x20000, so the ECU's own \
         boot code should still accept it."
    } else {
        "Nothing was written to the ECU. It is safe to try again."
    }
    .into())
}

pub(crate) fn run_flash(run: &Run, req: FlashRequest, assessment: Assessment) -> JobOutcome {
    let env = &run.env;
    let profile = env.profile;
    let tune_file = env.run_dir.join("tune.bin");
    if let Err(e) = fs::write(&tune_file, &assessment.tune) {
        return JobOutcome::failure(format!("Could not stage the file for flashing: {e}"), None);
    }
    let file = &assessment.preview.file;
    run.set_detail(format!(
        "{} sha256 {} on backup {}",
        file.name, file.sha256, req.backup_id
    ));

    let total = if req.verify_after { 2 } else { 1 };
    let cli = Cli {
        profile,
        device: &req.device,
        verbose: env.verbose,
    };

    run.phase("Flash", 1, total, PhaseMode::Flash);
    run.set_danger(true);
    let exec = run.exec(cli.flash(&tune_file));
    run.set_danger(false);
    let exec = match exec {
        Ok(exec) => exec,
        Err(e) => return JobOutcome::failure(e.to_string(), recovery_hint(run.flash_touched())),
    };
    if exec.cancelled {
        return JobOutcome {
            cancelled: true,
            message: "Flash stopped".into(),
            hint: recovery_hint(run.flash_touched()),
            ..JobOutcome::default()
        };
    }
    if !exec.ok {
        return JobOutcome::failure(exec.message, recovery_hint(run.flash_touched()));
    }
    if !req.verify_after {
        return JobOutcome::success("Flash finished. It was not read back, so it is not verified.");
    }

    let Some(backup) = assessment.backup.as_deref() else {
        return JobOutcome::success("Flash finished. No backup was available to verify against.");
    };
    run.log(format!(
        "Waiting {} s before the verification read",
        env.pass_gap.as_secs()
    ));
    if !run.pause(env.pass_gap) {
        return JobOutcome {
            cancelled: true,
            message: "Flashed, verification stopped".into(),
            hint: Some("Read the ECU again to check the result.".into()),
            ..JobOutcome::default()
        };
    }

    run.phase("Verify", 2, total, PhaseMode::Read);
    let (from, to) = profile.verify_range;
    let readback_file = env.run_dir.join("verify.bin");
    let unverified_hint =
        Some("The flash itself finished. Read the ECU again later to check it.".to_string());
    let exec = match run.exec(cli.read(&readback_file, from, to - from)) {
        Ok(exec) => exec,
        Err(e) => {
            return JobOutcome::failure(format!("Flashed, not verified: {e}"), unverified_hint);
        }
    };
    if exec.cancelled {
        return JobOutcome {
            cancelled: true,
            message: "Flashed, verification stopped".into(),
            hint: unverified_hint,
            ..JobOutcome::default()
        };
    }
    if !exec.ok {
        return JobOutcome::failure(
            format!("Flashed, not verified: {}", exec.message),
            unverified_hint,
        );
    }

    let readback = match fs::read(&readback_file) {
        Ok(data) => data,
        Err(e) => {
            return JobOutcome::failure(
                format!("Flashed, not verified: could not open the read-back: {e}"),
                unverified_hint,
            );
        }
    };
    let _ = fs::remove_file(&readback_file);
    let expected = assessment.prepared.as_deref().unwrap_or(&assessment.tune);
    let report = match profile::verify_readback(profile, expected, backup, &readback) {
        Ok(report) => report,
        Err(e) => {
            return JobOutcome::failure(format!("Flashed, not verified: {e}"), unverified_hint);
        }
    };

    if !report.written_ok {
        let bad: Vec<&str> = report
            .checks
            .iter()
            .filter(|c| c.expected == Expectation::Tune && c.mismatched > 0)
            .map(|c| c.name)
            .collect();
        return JobOutcome {
            ok: false,
            message: format!(
                "Flashed, but the read-back does not match your file in {}.",
                bad.join(" and ")
            ),
            hint: Some(
                "Flash the same file again and keep the ignition on until a flash is verified."
                    .into(),
            ),
            verify: Some(Box::new(report)),
            ..JobOutcome::default()
        };
    }
    let hint = report.untouched_changed.then(|| {
        "An area the flasher does not write (the gap) no longer matches your backup, so the erase probably covers it. \
         Keep your backup."
            .to_string()
    });
    JobOutcome {
        ok: true,
        message: "Flashed and verified. The written area matches your file byte for byte.".into(),
        hint,
        verify: Some(Box::new(report)),
        ..JobOutcome::default()
    }
}

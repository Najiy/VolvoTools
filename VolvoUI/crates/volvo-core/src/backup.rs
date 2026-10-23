//! Backup workflow: read the ECU (twice by default), compare the reads, and store the result.

use std::fs;

use crate::events::{JobOutcome, PhaseMode};
use crate::flasher::Cli;
use crate::hashing;
use crate::jobs::Run;
use crate::library::{BackupMeta, NewEntry};
use crate::profile;

const READ_HINT: &str = "Check that the ignition is on, the battery is charged and the adapter is connected, then try again. \
     If it keeps failing, switch the ignition off for a minute first.";

/// A stopped read can leave the ECU waiting in the flasher program until it loses power.
fn stopped() -> JobOutcome {
    JobOutcome {
        cancelled: true,
        message: "Backup stopped".into(),
        hint: Some(
            "The ECU may still be waiting in the flasher program. Switch the ignition off, wait a minute, \
             then on again before the next try."
                .into(),
        ),
        ..JobOutcome::default()
    }
}

/// Reads the whole image once into `read{pass}.bin` and returns its SHA-256.
fn read_pass(
    run: &Run,
    cli: &Cli,
    entry: &NewEntry,
    pass: u32,
    passes: u32,
) -> Result<String, JobOutcome> {
    let size = run.env.profile.image_size;
    run.phase(
        format!("Read {pass} of {passes}"),
        pass,
        passes,
        PhaseMode::Read,
    );
    let file = entry.dir.join(format!("read{pass}.bin"));

    let exec = run
        .exec(cli.read(&file, 0, size as u32))
        .map_err(|e| JobOutcome::failure(e.to_string(), None))?;
    if exec.cancelled {
        return Err(stopped());
    }
    if !exec.ok {
        return Err(JobOutcome::failure(exec.message, Some(READ_HINT.into())));
    }
    let (hash, len) = hashing::sha256_file(&file).map_err(|e| {
        JobOutcome::failure(
            format!("Could not open the file the flasher wrote: {e}"),
            None,
        )
    })?;
    if len != size as u64 {
        return Err(JobOutcome::failure(
            format!("The flasher reported success but wrote {len} bytes, expected {size}"),
            Some(READ_HINT.into()),
        ));
    }
    Ok(hash)
}

pub(crate) fn run_backup(run: &Run, device: &str, verify: bool) -> JobOutcome {
    let env = &run.env;
    let profile = env.profile;
    let entry = match env.library.create_entry(profile.id) {
        Ok(entry) => entry,
        Err(e) => return JobOutcome::failure(e.to_string(), None),
    };
    run.log(format!("Backup folder: {}", entry.dir.display()));

    let cli = Cli {
        profile,
        device,
        verbose: env.verbose,
    };
    let passes: u32 = if verify { 2 } else { 1 };
    let mut hashes: Vec<String> = Vec::new();
    let mut interrupted: Option<JobOutcome> = None;

    for pass in 1..=passes {
        if pass > 1 {
            run.log(format!(
                "Waiting {} s before the next read",
                env.pass_gap.as_secs()
            ));
            if !run.pause(env.pass_gap) {
                interrupted = Some(stopped());
                break;
            }
        }
        match read_pass(run, &cli, &entry, pass, passes) {
            Ok(hash) => hashes.push(hash),
            Err(outcome) => {
                interrupted = Some(outcome);
                break;
            }
        }
    }

    // With no complete read there is nothing worth keeping.
    let Some(first_hash) = hashes.first().cloned() else {
        env.library.discard(&entry);
        return interrupted.unwrap_or_else(|| JobOutcome::failure("Nothing was read", None));
    };

    let two_reads = hashes.len() == 2;
    let verified = two_reads && hashes[0] == hashes[1];
    if let Err(e) = fs::rename(entry.dir.join("read1.bin"), entry.image_path()) {
        return JobOutcome::failure(format!("Could not store the backup: {e}"), None);
    }
    let second = entry.dir.join("read2.bin");
    let mismatch = if two_reads && !verified {
        match (fs::read(entry.image_path()), fs::read(&second)) {
            (Ok(a), Ok(b)) => Some((
                profile::count_mismatches(&a, &b),
                profile::first_mismatch(&a, &b),
            )),
            _ => None,
        }
    } else {
        None
    };
    if verified {
        let _ = fs::remove_file(&second);
    }

    let meta = BackupMeta {
        id: entry.id.clone(),
        created_at: chrono::Local::now().to_rfc3339(),
        profile: profile.id.to_string(),
        size: profile.image_size as u64,
        sha256: first_hash.clone(),
        verified,
        reads: hashes,
        device: device.to_string(),
        note: String::new(),
    };
    if let Err(e) = env.library.write_meta(&entry, &meta) {
        return JobOutcome::failure(e.to_string(), None);
    }
    run.set_detail(format!("{} sha256 {first_hash}", entry.id));
    let backup = env.library.get(&entry.id).ok().map(Box::new);

    if verified {
        return JobOutcome {
            ok: true,
            message: "Backup verified. Two reads matched byte for byte.".into(),
            backup,
            ..JobOutcome::default()
        };
    }
    if let Some((count, first)) = mismatch {
        let at = first
            .map(|offset| format!(", first at 0x{offset:06X}"))
            .unwrap_or_default();
        return JobOutcome {
            message: format!("The two reads differ in {}{at}.", crate::text::bytes(count)),
            hint: Some(
                "Do not use this backup. Check the cable, charger and ignition, then read again."
                    .into(),
            ),
            backup,
            ..JobOutcome::default()
        };
    }
    match interrupted {
        Some(stopped) => JobOutcome {
            ok: false,
            cancelled: stopped.cancelled,
            message: format!(
                "{}. The first read was kept but is not verified.",
                stopped.message.trim_end_matches('.')
            ),
            hint: stopped.hint,
            backup,
            ..JobOutcome::default()
        },
        None => JobOutcome {
            ok: true,
            message:
                "Saved without a second read. This backup cannot be used as the base for a flash."
                    .into(),
            backup,
            ..JobOutcome::default()
        },
    }
}

//! Memory map of the supported target and the comparisons the safety gates are built on.

use serde::Serialize;

use crate::error::{CoreError, Result};
use crate::protocol::Plan;

/// Granularity of the sector strip shown in the UI.
pub const SECTOR: usize = 0x1_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RegionRole {
    /// Erased and rewritten by a flash.
    Written,
    /// Never part of the flash plan; changes made there by a tuner are ignored.
    NotWritten,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub name: &'static str,
    pub start: u32,
    /// Exclusive.
    pub end: u32,
    pub role: RegionRole,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: &'static str,
    pub title: &'static str,
    /// Value for the flasher's `-f`.
    pub platform: &'static str,
    /// Value for the flasher's `-e`.
    pub ecu: &'static str,
    /// Value for the flasher's `--cm-info`; selects the bootloader and the flash layout.
    pub cm_info: &'static str,
    pub baudrate: u32,
    pub image_size: usize,
    pub regions: &'static [Region],
    /// Erase list the flasher must report (start, length).
    pub erase: &'static [(u64, u64)],
    /// Write chunks the flasher must report (start, length).
    pub chunks: &'static [(u64, u64)],
    /// Range read back to verify a flash (start, exclusive end).
    pub verify_range: (u32, u32),
}

/// Volvo P1 with a Bosch ME9 engine computer (2 MiB flash), reached over D2 on CAN HS.
///
/// The layout mirrors `createVBFForME9P1` in Common/src/VBFUtil.cpp.
pub static ME9_P1: Profile = Profile {
    id: "p1-me9",
    title: "Volvo P1, Bosch ME9",
    platform: "P1",
    ecu: "7A",
    cm_info: "me9_p1",
    baudrate: 500_000,
    image_size: 0x20_0000,
    regions: &[
        Region {
            name: "Low block",
            start: 0x00_0000,
            end: 0x02_0000,
            role: RegionRole::NotWritten,
        },
        Region {
            name: "Block A",
            start: 0x02_0000,
            end: 0x09_0000,
            role: RegionRole::Written,
        },
        Region {
            name: "Gap",
            start: 0x09_0000,
            end: 0x0A_0000,
            role: RegionRole::NotWritten,
        },
        Region {
            name: "Block B",
            start: 0x0A_0000,
            end: 0x1F_0000,
            role: RegionRole::Written,
        },
        Region {
            name: "Tail",
            start: 0x1F_0000,
            end: 0x20_0000,
            role: RegionRole::NotWritten,
        },
    ],
    erase: &[(0x2_0000, 0)],
    chunks: &[(0x2_0000, 0x7_0000), (0xA_0000, 0x15_0000)],
    verify_range: (0x2_0000, 0x1F_0000),
};

impl Profile {
    pub fn written_bytes(&self) -> usize {
        self.regions
            .iter()
            .filter(|r| r.role == RegionRole::Written)
            .map(|r| (r.end - r.start) as usize)
            .sum()
    }

    /// Problems found when comparing a flasher-reported plan with what this target expects.
    /// An empty list means the plan is exactly the one the UI was designed around.
    pub fn plan_problems(&self, plan: &Plan) -> Vec<String> {
        let mut problems = Vec::new();
        if !plan.platform.eq_ignore_ascii_case(self.platform) {
            problems.push(format!(
                "Flasher planned for platform {}, expected {}",
                plan.platform, self.platform
            ));
        }
        if !plan.ecu.eq_ignore_ascii_case(self.ecu) {
            problems.push(format!(
                "Flasher planned for ECU {}, expected {}",
                plan.ecu, self.ecu
            ));
        }
        if plan.file_size != self.image_size as u64 {
            problems.push(format!(
                "Flasher saw a {} byte file, expected {}",
                plan.file_size, self.image_size
            ));
        }
        let erase: Vec<(u64, u64)> = plan.erase.iter().map(|s| (s.start, s.length)).collect();
        if erase != self.erase {
            problems.push(format!(
                "Unexpected erase list {erase:x?}, expected {:x?}",
                self.erase
            ));
        }
        let chunks: Vec<(u64, u64)> = plan.chunks.iter().map(|c| (c.start, c.length)).collect();
        if chunks != self.chunks {
            problems.push(format!(
                "Unexpected write list {chunks:x?}, expected {:x?}",
                self.chunks
            ));
        }
        problems
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionDiff {
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
    pub role: RegionRole,
    pub changed: u64,
    pub first_diff: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReport {
    pub regions: Vec<RegionDiff>,
    /// Changed bytes per 64 KiB sector, in address order.
    pub sectors: Vec<u32>,
    pub total_changed: u64,
    pub changed_in_written: u64,
    pub changed_in_not_written: u64,
    pub written_bytes: u64,
    pub identical: bool,
}

fn count_diff(a: &[u8], b: &[u8]) -> (u64, Option<usize>) {
    let mut count = 0u64;
    let mut first = None;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if x != y {
            count += 1;
            first.get_or_insert(i);
        }
    }
    (count, first)
}

fn check_size(profile: &Profile, data: &[u8], what: &str) -> Result<()> {
    if data.len() != profile.image_size {
        return Err(CoreError::Invalid(format!(
            "{what} is {} bytes, expected {}",
            data.len(),
            profile.image_size
        )));
    }
    Ok(())
}

pub fn diff(profile: &Profile, base: &[u8], candidate: &[u8]) -> Result<DiffReport> {
    check_size(profile, base, "Base image")?;
    check_size(profile, candidate, "Candidate image")?;

    let regions: Vec<RegionDiff> = profile
        .regions
        .iter()
        .map(|r| {
            let range = r.start as usize..r.end as usize;
            let (changed, first) = count_diff(&base[range.clone()], &candidate[range]);
            RegionDiff {
                name: r.name,
                start: r.start,
                end: r.end,
                role: r.role,
                changed,
                first_diff: first.map(|i| r.start + i as u32),
            }
        })
        .collect();

    let sectors: Vec<u32> = base
        .chunks(SECTOR)
        .zip(candidate.chunks(SECTOR))
        .map(|(a, b)| count_diff(a, b).0 as u32)
        .collect();

    let sum = |role: RegionRole| {
        regions
            .iter()
            .filter(|r| r.role == role)
            .map(|r| r.changed)
            .sum::<u64>()
    };
    let changed_in_written = sum(RegionRole::Written);
    let changed_in_not_written = sum(RegionRole::NotWritten);
    Ok(DiffReport {
        regions,
        sectors,
        total_changed: changed_in_written + changed_in_not_written,
        changed_in_written,
        changed_in_not_written,
        written_bytes: profile.written_bytes() as u64,
        identical: changed_in_written + changed_in_not_written == 0,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Expectation {
    /// Must equal the file that was flashed.
    Tune,
    /// Not written, so it should still equal the backup.
    Backup,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCheck {
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
    pub expected: Expectation,
    pub mismatched: u64,
    pub first_diff: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub checks: Vec<VerifyCheck>,
    /// Every written region matches the flashed file.
    pub written_ok: bool,
    /// A region that was not written no longer matches the backup.
    pub untouched_changed: bool,
}

/// Compares a read-back of `profile.verify_range` against the flashed file and the backup.
pub fn verify_readback(
    profile: &Profile,
    tune: &[u8],
    backup: &[u8],
    readback: &[u8],
) -> Result<VerifyReport> {
    check_size(profile, tune, "Flashed image")?;
    check_size(profile, backup, "Backup image")?;
    let (from, to) = profile.verify_range;
    let expected_len = (to - from) as usize;
    if readback.len() != expected_len {
        return Err(CoreError::Invalid(format!(
            "Read-back is {} bytes, expected {expected_len}",
            readback.len()
        )));
    }

    let mut checks = Vec::new();
    for r in profile.regions {
        let start = r.start.max(from);
        let end = r.end.min(to);
        if start >= end {
            continue;
        }
        let rb = &readback[(start - from) as usize..(end - from) as usize];
        let (expected, reference) = match r.role {
            RegionRole::Written => (Expectation::Tune, &tune[start as usize..end as usize]),
            RegionRole::NotWritten => (Expectation::Backup, &backup[start as usize..end as usize]),
        };
        let (mismatched, first) = count_diff(reference, rb);
        checks.push(VerifyCheck {
            name: r.name,
            start,
            end,
            expected,
            mismatched,
            first_diff: first.map(|i| start + i as u32),
        });
    }
    let written_ok = checks
        .iter()
        .filter(|c| c.expected == Expectation::Tune)
        .all(|c| c.mismatched == 0);
    let untouched_changed = checks
        .iter()
        .filter(|c| c.expected == Expectation::Backup)
        .any(|c| c.mismatched > 0);
    Ok(VerifyReport {
        checks,
        written_ok,
        untouched_changed,
    })
}

/// Index of the first byte that differs, if any (used to explain a failed backup comparison).
pub fn first_mismatch(a: &[u8], b: &[u8]) -> Option<usize> {
    if a.len() != b.len() {
        return Some(a.len().min(b.len()));
    }
    count_diff(a, b).1
}

/// Number of differing bytes between two buffers; a length difference counts as differing bytes.
pub fn count_mismatches(a: &[u8], b: &[u8]) -> u64 {
    count_diff(a, b).0 + a.len().abs_diff(b.len()) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BootloaderPlan, ChecksumInfo, ChunkPlan, Span};

    fn image(fill: u8) -> Vec<u8> {
        vec![fill; ME9_P1.image_size]
    }

    fn good_plan() -> Plan {
        Plan {
            platform: "P1".into(),
            ecu: "7A".into(),
            cm_info: "me9_p1".into(),
            file_size: ME9_P1.image_size as u64,
            checksum: ChecksumInfo {
                supported: true,
                valid: true,
                checked_blocks: 60,
            },
            bootloader: BootloaderPlan {
                call: 0x7F81D0,
                chunks: vec![Span {
                    start: 0x7F81D0,
                    length: 11496,
                }],
            },
            erase: vec![Span {
                start: 0x20000,
                length: 0,
            }],
            chunks: vec![
                ChunkPlan {
                    start: 0x20000,
                    length: 0x70000,
                    modified: 0,
                },
                ChunkPlan {
                    start: 0xA0000,
                    length: 0x150000,
                    modified: 0,
                },
            ],
        }
    }

    #[test]
    fn regions_tile_the_image_without_gaps() {
        let mut next = 0;
        for r in ME9_P1.regions {
            assert_eq!(
                r.start, next,
                "{} starts where the previous one ended",
                r.name
            );
            next = r.end;
        }
        assert_eq!(next as usize, ME9_P1.image_size);
    }

    #[test]
    fn written_regions_match_the_expected_chunks() {
        let written: Vec<(u64, u64)> = ME9_P1
            .regions
            .iter()
            .filter(|r| r.role == RegionRole::Written)
            .map(|r| (r.start as u64, (r.end - r.start) as u64))
            .collect();
        assert_eq!(written, ME9_P1.chunks);
        assert_eq!(ME9_P1.written_bytes(), 0x1C_0000);
    }

    #[test]
    fn identical_images_report_no_difference() {
        let a = image(0xAB);
        let report = diff(&ME9_P1, &a, &a).unwrap();
        assert!(report.identical);
        assert_eq!(report.total_changed, 0);
        assert_eq!(report.sectors.len(), 32);
    }

    #[test]
    fn diff_attributes_changes_to_regions_and_sectors() {
        let base = image(0);
        let mut tune = base.clone();
        tune[0x0A_0010] ^= 0xFF; // Block B
        tune[0x0A_0011] ^= 0xFF; // Block B
        tune[0x09_0000] ^= 0x01; // Gap (not written)
        let report = diff(&ME9_P1, &base, &tune).unwrap();
        assert_eq!(report.total_changed, 3);
        assert_eq!(report.changed_in_written, 2);
        assert_eq!(report.changed_in_not_written, 1);
        let block_b = report.regions.iter().find(|r| r.name == "Block B").unwrap();
        assert_eq!((block_b.changed, block_b.first_diff), (2, Some(0x0A_0010)));
        assert_eq!(report.sectors[0xA], 2);
        assert_eq!(report.sectors[0x9], 1);
        assert_eq!(report.sectors.iter().sum::<u32>(), 3);
    }

    #[test]
    fn diff_rejects_wrong_sizes() {
        let a = image(0);
        assert!(diff(&ME9_P1, &a, &a[..a.len() - 1]).is_err());
        assert!(diff(&ME9_P1, &a[..10], &a).is_err());
    }

    #[test]
    fn the_expected_plan_has_no_problems() {
        assert!(ME9_P1.plan_problems(&good_plan()).is_empty());
    }

    #[test]
    fn deviating_plans_are_reported() {
        let mut plan = good_plan();
        plan.chunks[0].start = 0; // would write over the low block
        assert!(
            ME9_P1
                .plan_problems(&plan)
                .iter()
                .any(|p| p.contains("write list"))
        );

        let mut plan = good_plan();
        plan.erase[0].start = 0;
        assert!(
            ME9_P1
                .plan_problems(&plan)
                .iter()
                .any(|p| p.contains("erase list"))
        );

        let mut plan = good_plan();
        plan.platform = "P2".into();
        plan.file_size = 1024;
        let problems = ME9_P1.plan_problems(&plan);
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn verify_passes_when_written_regions_match() {
        let backup = image(0x11);
        let mut tune = backup.clone();
        tune[0x02_0000..0x09_0000].fill(0x22);
        tune[0x0A_0000..0x1F_0000].fill(0x33);
        let (from, to) = ME9_P1.verify_range;
        let mut readback = tune[from as usize..to as usize].to_vec();
        // The gap in the read-back still holds the backup's content.
        let gap = (0x09_0000 - from) as usize..(0x0A_0000 - from) as usize;
        readback[gap].fill(0x11);
        let report = verify_readback(&ME9_P1, &tune, &backup, &readback).unwrap();
        assert!(report.written_ok);
        assert!(!report.untouched_changed);
        assert_eq!(
            report.checks.len(),
            3,
            "Block A, Gap, Block B lie inside the verify range"
        );
    }

    #[test]
    fn verify_flags_a_bad_write_and_a_wiped_gap() {
        let backup = image(0x11);
        let mut tune = backup.clone();
        tune[0x0A_0000..0x1F_0000].fill(0x33);
        let (from, to) = ME9_P1.verify_range;
        let mut readback = tune[from as usize..to as usize].to_vec();
        readback[(0x0B_0000 - from) as usize] ^= 0x80; // one bad byte in Block B
        let gap = (0x09_0000 - from) as usize..(0x0A_0000 - from) as usize;
        readback[gap].fill(0xFF); // erase also wiped the gap
        let report = verify_readback(&ME9_P1, &tune, &backup, &readback).unwrap();
        assert!(!report.written_ok);
        assert!(report.untouched_changed);
        let block_b = report.checks.iter().find(|c| c.name == "Block B").unwrap();
        assert_eq!(
            (block_b.mismatched, block_b.first_diff),
            (1, Some(0x0B_0000))
        );
    }

    #[test]
    fn mismatch_helpers() {
        assert_eq!(first_mismatch(&[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(first_mismatch(&[1, 2, 3], &[1, 9, 3]), Some(1));
        assert_eq!(first_mismatch(&[1, 2], &[1, 2, 3]), Some(2));
        assert_eq!(count_mismatches(&[1, 2, 3], &[9, 2, 9]), 2);
        assert_eq!(count_mismatches(&[1, 2], &[1, 2, 3, 4]), 2);
    }
}

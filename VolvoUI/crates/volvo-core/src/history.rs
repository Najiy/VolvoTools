//! Append-only record of finished jobs, one JSON object per line.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::events::JobKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub time: String,
    pub kind: JobKind,
    pub ok: bool,
    pub message: String,
    /// File names and checksums that identify what was read or written.
    pub detail: String,
    pub duration_s: u64,
}

/// Best effort: a failure to write the history must never fail a job.
pub fn append(path: &Path, entry: &HistoryEntry) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// The most recent `limit` entries, newest first.
pub fn recent(path: &Path, limit: usize) -> Vec<HistoryEntry> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<HistoryEntry>(line).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(message: &str) -> HistoryEntry {
        HistoryEntry {
            time: "2026-10-23T10:00:00+00:00".into(),
            kind: JobKind::Flash,
            ok: true,
            message: message.into(),
            detail: String::new(),
            duration_s: 3,
        }
    }

    #[test]
    fn appends_and_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("history.jsonl");
        assert!(recent(&path, 10).is_empty());
        for name in ["a", "b", "c"] {
            append(&path, &entry(name));
        }
        let got: Vec<String> = recent(&path, 2).into_iter().map(|e| e.message).collect();
        assert_eq!(got, ["c", "b"]);
    }

    #[test]
    fn damaged_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        append(&path, &entry("good"));
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        assert_eq!(recent(&path, 10).len(), 1);
    }
}

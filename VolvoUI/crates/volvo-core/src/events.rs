//! Types streamed to the UI while a job runs, and the snapshot that lets a reloaded UI catch up.

use serde::{Deserialize, Serialize};

use crate::library::BackupEntry;
use crate::profile::VerifyReport;
use crate::runner::LogStream;

const LOG_TAIL: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    Backup,
    Flash,
}

/// Tells the UI which stage list applies. The flasher reports the same state names for
/// reading and writing, so the meaning of "WriteFlash" depends on the phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PhaseMode {
    Read,
    Flash,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: u64,
    pub kind: JobKind,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseInfo {
    pub label: String,
    pub index: u32,
    pub total: u32,
    pub mode: PhaseMode,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProgressInfo {
    pub current: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub stream: LogStream,
    pub line: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOutcome {
    pub ok: bool,
    pub cancelled: bool,
    pub message: String,
    /// What to do next, shown under the message.
    pub hint: Option<String>,
    // Boxed so that a plain failure stays small; these are large and rarely present.
    pub backup: Option<Box<BackupEntry>>,
    pub verify: Option<Box<VerifyReport>>,
}

impl JobOutcome {
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            ..Self::default()
        }
    }

    pub fn failure(message: impl Into<String>, hint: Option<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            hint,
            ..Self::default()
        }
    }

    pub fn stopped(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            cancelled: true,
            message: message.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum JobEvent {
    Started { job: JobInfo },
    Phase(PhaseInfo),
    State { state: String },
    Progress(ProgressInfo),
    Log(LogLine),
    Finished { outcome: JobOutcome },
}

/// Receives job events. The Tauri layer forwards them to the webview.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &JobEvent);
}

/// The latest state of the current or most recent job.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshot {
    pub running: bool,
    pub job: Option<JobInfo>,
    pub phase: Option<PhaseInfo>,
    pub state: Option<String>,
    pub progress: Option<ProgressInfo>,
    pub log: Vec<LogLine>,
    pub outcome: Option<JobOutcome>,
}

impl JobSnapshot {
    pub fn apply(&mut self, event: &JobEvent) {
        match event {
            JobEvent::Started { job } => {
                *self = JobSnapshot {
                    running: true,
                    job: Some(job.clone()),
                    ..JobSnapshot::default()
                };
            }
            JobEvent::Phase(phase) => {
                self.phase = Some(phase.clone());
                self.state = None;
                self.progress = None;
            }
            JobEvent::State { state } => self.state = Some(state.clone()),
            JobEvent::Progress(progress) => self.progress = Some(*progress),
            JobEvent::Log(line) => {
                self.log.push(line.clone());
                if self.log.len() > LOG_TAIL {
                    self.log.drain(..self.log.len() - LOG_TAIL);
                }
            }
            JobEvent::Finished { outcome } => {
                self.running = false;
                self.outcome = Some(outcome.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> JobEvent {
        JobEvent::Started {
            job: JobInfo {
                id: 1,
                kind: JobKind::Backup,
                started_at_ms: 0,
            },
        }
    }

    #[test]
    fn snapshot_follows_events_and_resets_per_phase() {
        let mut s = JobSnapshot::default();
        s.apply(&started());
        s.apply(&JobEvent::Phase(PhaseInfo {
            label: "Read 1 of 2".into(),
            index: 1,
            total: 2,
            mode: PhaseMode::Read,
        }));
        s.apply(&JobEvent::State {
            state: "WriteFlash".into(),
        });
        s.apply(&JobEvent::Progress(ProgressInfo {
            current: 5,
            max: 10,
        }));
        assert!(s.running);
        assert_eq!(s.state.as_deref(), Some("WriteFlash"));
        assert_eq!(s.progress.map(|p| p.current), Some(5));

        s.apply(&JobEvent::Phase(PhaseInfo {
            label: "Read 2 of 2".into(),
            index: 2,
            total: 2,
            mode: PhaseMode::Read,
        }));
        assert!(s.state.is_none() && s.progress.is_none());

        s.apply(&JobEvent::Finished {
            outcome: JobOutcome::success("ok"),
        });
        assert!(!s.running);
        assert!(s.outcome.as_ref().is_some_and(|o| o.ok));
    }

    #[test]
    fn a_new_job_clears_the_previous_snapshot() {
        let mut s = JobSnapshot::default();
        s.apply(&started());
        s.apply(&JobEvent::Log(LogLine {
            stream: LogStream::App,
            line: "old".into(),
        }));
        s.apply(&JobEvent::Finished {
            outcome: JobOutcome::success("done"),
        });
        s.apply(&started());
        assert!(s.log.is_empty() && s.outcome.is_none() && s.running);
    }

    #[test]
    fn log_is_capped_to_the_tail() {
        let mut s = JobSnapshot::default();
        for i in 0..(LOG_TAIL + 50) {
            s.apply(&JobEvent::Log(LogLine {
                stream: LogStream::Stdout,
                line: i.to_string(),
            }));
        }
        assert_eq!(s.log.len(), LOG_TAIL);
        assert_eq!(s.log.first().unwrap().line, "50");
        assert_eq!(s.log.last().unwrap().line, (LOG_TAIL + 49).to_string());
    }

    #[test]
    fn events_serialize_with_a_kind_tag() {
        let json =
            serde_json::to_value(JobEvent::Progress(ProgressInfo { current: 1, max: 2 })).unwrap();
        assert_eq!(json["kind"], "progress");
        assert_eq!(json["current"], 1);
        let json = serde_json::to_value(JobEvent::Phase(PhaseInfo {
            label: "x".into(),
            index: 1,
            total: 1,
            mode: PhaseMode::Flash,
        }))
        .unwrap();
        assert_eq!(
            (json["kind"].as_str(), json["mode"].as_str()),
            (Some("phase"), Some("flash"))
        );
    }
}

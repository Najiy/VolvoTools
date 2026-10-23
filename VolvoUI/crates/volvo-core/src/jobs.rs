//! Job plumbing: exclusive execution, event fan-out, cancellation, and running the flasher.

use std::ffi::OsString;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{CoreError, Result};
use crate::events::{
    EventSink, JobEvent, JobInfo, JobKind, JobOutcome, JobSnapshot, LogLine, PhaseInfo, PhaseMode,
    ProgressInfo,
};
use crate::history::{self, HistoryEntry};
use crate::library::Library;
use crate::profile::Profile;
use crate::protocol::{Message, PROTOCOL_VERSION};
use crate::runner::{self, CancelToken, Flow, Invocation, LogStream, Output};

/// Everything a running job needs, captured when it starts so later setting changes cannot affect it.
pub(crate) struct Env {
    pub exe: PathBuf,
    pub profile: &'static Profile,
    pub library: Library,
    /// Working directory of the flasher; its `application.log` ends up here.
    pub run_dir: PathBuf,
    pub history: PathBuf,
    /// Pause between the two reads of a backup so the car's network can settle.
    pub pass_gap: Duration,
    pub verbose: bool,
    /// Extra environment variables for every flasher process of this job.
    pub flasher_env: Vec<(String, String)>,
}

struct ActiveJob {
    cancel: CancelToken,
}

/// State shared between the job thread and the commands that observe or stop it.
pub(crate) struct Shared {
    snapshot: Mutex<JobSnapshot>,
    active: Mutex<Option<ActiveJob>>,
    next_id: AtomicU64,
    /// The flasher may currently be changing the ECU's flash.
    danger: AtomicBool,
    /// A flash reached its erase or write stage.
    flash_touched: AtomicBool,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            snapshot: Mutex::new(JobSnapshot::default()),
            active: Mutex::new(None),
            next_id: AtomicU64::new(1),
            danger: AtomicBool::new(false),
            flash_touched: AtomicBool::new(false),
        }
    }

    pub fn snapshot(&self) -> JobSnapshot {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn is_busy(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    /// Stops the running job. While the flash is being erased or written a plain stop is refused,
    /// because interrupting that leaves the ECU without a valid program.
    pub fn request_cancel(&self, force: bool) -> Result<()> {
        let active = self.active.lock().unwrap();
        let Some(job) = active.as_ref() else {
            return Err(CoreError::NotRunning);
        };
        if !force && self.danger.load(Ordering::SeqCst) {
            let state = self.snapshot.lock().unwrap().state.clone();
            if matches!(state.as_deref(), Some("EraseFlash" | "WriteFlash")) {
                return Err(CoreError::Refused(
                    "The ECU is being erased or written. Stopping now can leave it without a valid program. \
                     Use Force stop only if the flasher is stuck."
                        .into(),
                ));
            }
        }
        job.cancel.cancel();
        Ok(())
    }
}

pub(crate) struct Exec {
    pub ok: bool,
    pub cancelled: bool,
    pub message: String,
}

/// The handle a workflow uses to talk to the UI and to run the flasher.
pub(crate) struct Run {
    pub env: Env,
    sink: Arc<dyn EventSink>,
    shared: Arc<Shared>,
    cancel: CancelToken,
    detail: Mutex<String>,
}

impl Run {
    pub fn emit(&self, event: JobEvent) {
        self.shared.snapshot.lock().unwrap().apply(&event);
        self.sink.emit(&event);
    }

    pub fn log(&self, line: impl Into<String>) {
        self.emit(JobEvent::Log(LogLine {
            stream: LogStream::App,
            line: line.into(),
        }));
    }

    pub fn phase(&self, label: impl Into<String>, index: u32, total: u32, mode: PhaseMode) {
        let label = label.into();
        self.log(format!("Step {index} of {total}: {label}"));
        self.emit(JobEvent::Phase(PhaseInfo {
            label,
            index,
            total,
            mode,
        }));
    }

    pub fn set_detail(&self, detail: impl Into<String>) {
        *self.detail.lock().unwrap() = detail.into();
    }

    pub fn set_danger(&self, on: bool) {
        self.shared.danger.store(on, Ordering::SeqCst);
    }

    /// The flash stage reached erase or write, so the ECU's program may be incomplete.
    pub fn flash_touched(&self) -> bool {
        self.shared.flash_touched.load(Ordering::SeqCst)
    }

    /// Sleeps in short slices. Returns false if the job was cancelled meanwhile.
    pub fn pause(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if self.cancel.is_cancelled() {
                return false;
            }
            thread::sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        !self.cancel.is_cancelled()
    }

    /// Runs the flasher with `args`, forwarding its progress to the UI.
    pub fn exec(&self, args: Vec<OsString>) -> Result<Exec> {
        let inv = Invocation {
            exe: self.env.exe.clone(),
            args,
            cwd: self.env.run_dir.clone(),
            env: self.env.flasher_env.clone(),
        };
        self.log(format!("> {}", inv.display()));

        let mut result: Option<(bool, String)> = None;
        let mut wrong_protocol: Option<u32> = None;
        let outcome = runner::run(&inv, &self.cancel, None, |output| {
            match output {
                Output::Message(Message::Hello { protocol, .. })
                    if protocol != PROTOCOL_VERSION =>
                {
                    wrong_protocol = Some(protocol);
                    return Flow::Kill;
                }
                Output::Message(Message::State { state }) => {
                    if self.shared.danger.load(Ordering::SeqCst)
                        && matches!(state.as_str(), "EraseFlash" | "WriteFlash")
                    {
                        self.shared.flash_touched.store(true, Ordering::SeqCst);
                    }
                    self.log(format!("Stage: {state}"));
                    self.emit(JobEvent::State { state });
                }
                Output::Message(Message::Progress { current, max }) => {
                    self.emit(JobEvent::Progress(ProgressInfo { current, max }));
                }
                Output::Message(Message::Result { ok, message }) => result = Some((ok, message)),
                Output::Message(_) => {}
                Output::Raw { stream, line } => self.emit(JobEvent::Log(LogLine { stream, line })),
            }
            Flow::Continue
        })?;

        if let Some(protocol) = wrong_protocol {
            return Ok(Exec {
                ok: false,
                cancelled: false,
                message: format!(
                    "VolvoFlasher speaks protocol {protocol}, this app needs {PROTOCOL_VERSION}. Rebuild one of them."
                ),
            });
        }
        if outcome.cancelled {
            return Ok(Exec {
                ok: false,
                cancelled: true,
                message: "Stopped".into(),
            });
        }
        let exited = match outcome.exit_code {
            Some(code) => format!("with code {code}"),
            None => "abnormally".to_string(),
        };
        let (ok, message) = match result {
            Some((true, message)) if outcome.exit_code == Some(0) => (true, message),
            Some((true, message)) => (false, format!("{message} (the flasher exited {exited})")),
            Some((false, message)) => (false, message),
            None => (
                false,
                format!("The flasher exited {exited} without reporting a result"),
            ),
        };
        self.log(format!(
            "{}: {message}",
            if ok { "Finished" } else { "Failed" }
        ));
        Ok(Exec {
            ok,
            cancelled: false,
            message,
        })
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Starts `work` on its own thread as the one running job. The result is announced with a
/// `Finished` event after the job slot has been freed, so the UI can start the next job at once.
pub(crate) fn spawn<F>(
    shared: &Arc<Shared>,
    sink: Arc<dyn EventSink>,
    env: Env,
    kind: JobKind,
    work: F,
) -> Result<JobInfo>
where
    F: FnOnce(&Run) -> JobOutcome + Send + 'static,
{
    let cancel = CancelToken::default();
    let info = {
        let mut active = shared.active.lock().unwrap();
        if active.is_some() {
            return Err(CoreError::Busy);
        }
        *active = Some(ActiveJob {
            cancel: cancel.clone(),
        });
        JobInfo {
            id: shared.next_id.fetch_add(1, Ordering::SeqCst),
            kind,
            started_at_ms: now_ms(),
        }
    };
    shared.danger.store(false, Ordering::SeqCst);
    shared.flash_touched.store(false, Ordering::SeqCst);

    let run = Run {
        env,
        sink,
        shared: shared.clone(),
        cancel,
        detail: Mutex::new(String::new()),
    };
    run.emit(JobEvent::Started { job: info.clone() });

    let thread_shared = shared.clone();
    let spawned = thread::Builder::new().name(format!("job-{}", info.id)).spawn(move || {
        let started = Instant::now();
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| work(&run))).unwrap_or_else(|_| {
            JobOutcome::failure(
                "Internal error: the job stopped unexpectedly",
                Some("The activity log has details. If the ECU was being written, run the flash again.".into()),
            )
        });
        run.set_danger(false);
        history::append(
            &run.env.history,
            &HistoryEntry {
                time: chrono::Local::now().to_rfc3339(),
                kind,
                ok: outcome.ok,
                message: outcome.message.clone(),
                detail: run.detail.lock().unwrap().clone(),
                duration_s: started.elapsed().as_secs(),
            },
        );
        *thread_shared.active.lock().unwrap() = None;
        run.emit(JobEvent::Finished { outcome });
    });

    if let Err(source) = spawned {
        *shared.active.lock().unwrap() = None;
        return Err(CoreError::Io {
            context: "starting the job thread".into(),
            source,
        });
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_without_a_job_is_an_error() {
        assert!(matches!(
            Shared::new().request_cancel(false),
            Err(CoreError::NotRunning)
        ));
    }

    #[test]
    fn plain_stop_is_refused_while_the_flash_is_written_but_force_works() {
        let shared = Shared::new();
        let token = CancelToken::default();
        *shared.active.lock().unwrap() = Some(ActiveJob {
            cancel: token.clone(),
        });
        shared.danger.store(true, Ordering::SeqCst);
        shared.snapshot.lock().unwrap().state = Some("WriteFlash".into());

        let refused = shared.request_cancel(false).unwrap_err();
        assert!(matches!(refused, CoreError::Refused(_)));
        assert!(!token.is_cancelled());

        shared.request_cancel(true).unwrap();
        assert!(token.is_cancelled());
    }

    #[test]
    fn plain_stop_works_outside_the_dangerous_stages() {
        let shared = Shared::new();
        let token = CancelToken::default();
        *shared.active.lock().unwrap() = Some(ActiveJob {
            cancel: token.clone(),
        });
        shared.danger.store(true, Ordering::SeqCst);
        shared.snapshot.lock().unwrap().state = Some("LoadBootloader".into());
        shared.request_cancel(false).unwrap();
        assert!(token.is_cancelled());
    }

    #[test]
    fn reads_report_the_same_stage_names_but_are_never_dangerous() {
        // Reading also passes through "WriteFlash" in the flasher, but the danger flag is only set
        // by the flash workflow, so a backup can always be stopped with a plain stop.
        let shared = Shared::new();
        let token = CancelToken::default();
        *shared.active.lock().unwrap() = Some(ActiveJob {
            cancel: token.clone(),
        });
        shared.snapshot.lock().unwrap().state = Some("WriteFlash".into());
        shared.request_cancel(false).unwrap();
        assert!(token.is_cancelled());
    }
}

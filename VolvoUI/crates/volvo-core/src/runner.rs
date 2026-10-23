//! Runs the flasher as a child process and turns its output into a stream of events.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{CoreError, Result};
use crate::protocol::{self, Line, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LogStream {
    Stdout,
    Stderr,
    /// Written by this app, not by the flasher.
    App,
}

#[derive(Debug, Clone)]
pub struct Invocation {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// Extra environment variables for the child, on top of the inherited ones.
    pub env: Vec<(String, String)>,
}

impl Invocation {
    /// The command line as it would be typed, for the activity log.
    pub fn display(&self) -> String {
        let quote = |s: &str| {
            if s.contains(' ') {
                format!("\"{s}\"")
            } else {
                s.to_string()
            }
        };
        let mut parts = vec![quote(&self.exe.display().to_string())];
        parts.extend(self.args.iter().map(|a| quote(&a.to_string_lossy())));
        parts.join(" ")
    }
}

#[derive(Debug, Clone)]
pub enum Output {
    Message(Message),
    Raw { stream: LogStream, line: String },
}

/// What the output handler wants to happen next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Kill,
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    /// Killed because the cancel token was set.
    pub cancelled: bool,
    pub timed_out: bool,
    /// Killed because the output handler asked for it.
    pub killed_by_handler: bool,
}

fn spawn_reader<R: Read + Send + 'static>(
    stream: R,
    which: LogStream,
    tx: mpsc::Sender<Output>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&buf);
            let text = text.trim_end_matches(['\r', '\n']);
            if text.trim().is_empty() {
                continue;
            }
            let output = match (which, protocol::parse_line(text)) {
                (LogStream::Stdout, Line::Message(m)) => Output::Message(m),
                _ => Output::Raw {
                    stream: which,
                    line: text.to_string(),
                },
            };
            if tx.send(output).is_err() {
                break;
            }
        }
    })
}

/// Runs `inv` to completion, calling `on_output` for every line on the calling thread.
///
/// Setting `cancel` or exceeding `timeout` kills the process; the call still returns normally
/// with the matching flag set so the caller can report what happened.
pub fn run(
    inv: &Invocation,
    cancel: &CancelToken,
    timeout: Option<Duration>,
    mut on_output: impl FnMut(Output) -> Flow,
) -> Result<Outcome> {
    let mut command = Command::new(&inv.exe);
    command
        .args(&inv.args)
        .current_dir(&inv.cwd)
        .envs(inv.env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|source| CoreError::Spawn {
        path: inv.exe.display().to_string(),
        source,
    })?;

    let (tx, rx) = mpsc::channel();
    let readers = [
        spawn_reader(
            child.stdout.take().expect("stdout is piped"),
            LogStream::Stdout,
            tx.clone(),
        ),
        spawn_reader(
            child.stderr.take().expect("stderr is piped"),
            LogStream::Stderr,
            tx,
        ),
    ];

    let started = Instant::now();
    let mut outcome = Outcome::default();
    let mut killed = false;
    let kill = |child: &mut std::process::Child, killed: &mut bool| {
        if !*killed {
            let _ = child.kill();
            *killed = true;
        }
    };

    loop {
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(output) => {
                if on_output(output) == Flow::Kill {
                    outcome.killed_by_handler = true;
                    kill(&mut child, &mut killed);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if cancel.is_cancelled() && !killed {
            outcome.cancelled = true;
            kill(&mut child, &mut killed);
        }
        if let Some(limit) = timeout
            && started.elapsed() > limit
            && !killed
        {
            outcome.timed_out = true;
            kill(&mut child, &mut killed);
        }
    }

    for reader in readers {
        let _ = reader.join();
    }
    outcome.exit_code = child.wait().ok().and_then(|status| status.code());
    Ok(outcome)
}

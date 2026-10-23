//! Locating VolvoFlasher.exe, checking that it speaks our protocol, and building its arguments.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::profile::Profile;
use crate::protocol::{Device, Message, PROTOCOL_VERSION};
use crate::runner::{self, CancelToken, Flow, Invocation, Output};

/// Environment variable the app reads to override the configured flasher path,
/// for example to try the UI against the simulator (`npm run dev:sim`).
pub const FLASHER_ENV: &str = "VOLVOUI_FLASHER";
const EXE_NAME: &str = "VolvoFlasher.exe";
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Arguments for the flasher. Everything runs in `--json` mode.
pub struct Cli<'a> {
    pub profile: &'a Profile,
    /// Empty for commands that never touch hardware, such as the dry run.
    pub device: &'a str,
    pub verbose: bool,
}

impl Cli<'_> {
    fn base(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["--json".into()];
        if self.verbose {
            args.push("-v".into());
        }
        if !self.device.is_empty() {
            args.extend(["-d".into(), self.device.into()]);
        }
        args.extend([
            "-b".into(),
            self.profile.baudrate.to_string().into(),
            "-f".into(),
            self.profile.platform.into(),
            "-e".into(),
            self.profile.ecu.into(),
            "--cm-info".into(),
            self.profile.cm_info.into(),
        ]);
        args
    }

    /// Start and size are hex without a prefix, which is what the flasher's parser expects.
    pub fn read(&self, output: &Path, start: u32, size: u32) -> Vec<OsString> {
        let mut args = self.base();
        args.extend([
            "read".into(),
            "-o".into(),
            output.into(),
            "-s".into(),
            format!("{start:X}").into(),
            "-sz".into(),
            format!("{size:X}").into(),
        ]);
        args
    }

    pub fn flash(&self, input: &Path) -> Vec<OsString> {
        let mut args = self.base();
        args.extend(["flash".into(), "-i".into(), input.into()]);
        args
    }

    /// Asks the flasher what it would write. `image_out` receives the exact image it would send,
    /// including any checksum corrections, so a read-back can be compared against the right bytes.
    pub fn dry_run(&self, input: &Path, image_out: Option<&Path>) -> Vec<OsString> {
        let mut args = self.flash(input);
        args.push("--dry-run".into());
        if let Some(path) = image_out {
            args.extend(["--emit-image".into(), path.into()]);
        }
        args
    }
}

pub fn devices_args() -> Vec<OsString> {
    vec!["--json".into(), "devices".into()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Env,
    Settings,
    Auto,
}

/// Looks for a built flasher next to the app and in the repository's CMake build tree.
pub fn discover(app_exe_dir: Option<&Path>) -> Option<PathBuf> {
    let start = app_exe_dir?;
    let beside = start.join(EXE_NAME);
    if beside.is_file() {
        return Some(beside);
    }
    const BUILD_DIRS: [&str; 3] = ["Release", "RelWithDebInfo", "Debug"];
    start.ancestors().take(8).find_map(|dir| {
        BUILD_DIRS
            .iter()
            .map(|config| {
                dir.join("build")
                    .join("VolvoFlasher")
                    .join(config)
                    .join(EXE_NAME)
            })
            .find(|candidate| candidate.is_file())
    })
}

/// An explicit override wins over the saved setting, which wins over automatic discovery.
pub fn resolve(
    override_path: Option<&Path>,
    configured: Option<&str>,
    app_exe_dir: Option<&Path>,
) -> Option<(PathBuf, Source)> {
    if let Some(path) = override_path {
        return Some((path.to_path_buf(), Source::Env));
    }
    if let Some(path) = configured.filter(|p| !p.trim().is_empty()) {
        return Some((PathBuf::from(path), Source::Settings));
    }
    discover(app_exe_dir).map(|path| (path, Source::Auto))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlasherStatus {
    pub path: Option<String>,
    pub source: Option<Source>,
    pub found: bool,
    pub compatible: bool,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub flasher: FlasherStatus,
    pub devices: Vec<Device>,
}

impl ScanResult {
    fn unusable(
        path: Option<&Path>,
        source: Option<Source>,
        found: bool,
        error: impl Into<String>,
    ) -> Self {
        Self {
            flasher: FlasherStatus {
                path: path.map(|p| p.display().to_string()),
                source,
                found,
                compatible: false,
                version: None,
                error: Some(error.into()),
            },
            devices: Vec::new(),
        }
    }
}

/// Runs `VolvoFlasher --json devices`, which doubles as the compatibility handshake.
pub fn probe(
    exe: Option<(PathBuf, Source)>,
    scratch: &Path,
    env: &[(String, String)],
) -> ScanResult {
    let Some((exe, source)) = exe else {
        return ScanResult::unusable(
            None,
            None,
            false,
            "VolvoFlasher.exe was not found. Build it (see VolvoUI/README.md) or choose its location.",
        );
    };
    if !exe.is_file() {
        return ScanResult::unusable(
            Some(&exe),
            Some(source),
            false,
            "There is no file at this path.",
        );
    }

    let inv = Invocation {
        exe: exe.clone(),
        args: devices_args(),
        cwd: scratch.to_path_buf(),
        env: env.to_vec(),
    };
    let mut hello: Option<(u32, String)> = None;
    let mut devices = Vec::new();
    let mut result: Option<(bool, String)> = None;
    let mut first_raw: Option<String> = None;
    let outcome = runner::run(
        &inv,
        &CancelToken::default(),
        Some(PROBE_TIMEOUT),
        |output| {
            match output {
                Output::Raw { line, .. } => {
                    first_raw.get_or_insert(line);
                }
                Output::Message(Message::Hello {
                    protocol, version, ..
                }) => hello = Some((protocol, version)),
                Output::Message(Message::Devices { devices: found }) => devices = found,
                Output::Message(Message::Result { ok, message }) => result = Some((ok, message)),
                _ => {}
            }
            Flow::Continue
        },
    );

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => return ScanResult::unusable(Some(&exe), Some(source), true, e.to_string()),
    };
    if outcome.timed_out {
        return ScanResult::unusable(
            Some(&exe),
            Some(source),
            true,
            "The flasher did not answer within 20 seconds.",
        );
    }
    let Some((protocol, version)) = hello else {
        let printed = first_raw
            .map(|line| format!(" It printed: {line}"))
            .unwrap_or_default();
        return ScanResult::unusable(
            Some(&exe),
            Some(source),
            true,
            format!(
                "This VolvoFlasher build does not support the UI protocol. Rebuild it from this repository.{printed}"
            ),
        );
    };
    if protocol != PROTOCOL_VERSION {
        return ScanResult::unusable(
            Some(&exe),
            Some(source),
            true,
            format!(
                "VolvoFlasher speaks protocol {protocol}, this app needs {PROTOCOL_VERSION}. Rebuild one of them."
            ),
        );
    }
    if let Some((false, message)) = result {
        return ScanResult::unusable(Some(&exe), Some(source), true, message);
    }

    ScanResult {
        flasher: FlasherStatus {
            path: Some(exe.display().to_string()),
            source: Some(source),
            found: true,
            compatible: true,
            version: Some(version).filter(|v| !v.is_empty()),
            error: None,
        },
        devices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ME9_P1;
    use std::fs;

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn read_arguments_are_hex_without_prefix() {
        let cli = Cli {
            profile: &ME9_P1,
            device: "DiCE-1",
            verbose: false,
        };
        let args = strings(cli.read(Path::new("out.bin"), 0x2_0000, 0x1D_0000));
        assert_eq!(
            args,
            [
                "--json",
                "-d",
                "DiCE-1",
                "-b",
                "500000",
                "-f",
                "P1",
                "-e",
                "7A",
                "--cm-info",
                "me9_p1",
                "read",
                "-o",
                "out.bin",
                "-s",
                "20000",
                "-sz",
                "1D0000"
            ]
        );
    }

    #[test]
    fn dry_run_has_no_device_and_verbose_goes_before_the_subcommand() {
        let cli = Cli {
            profile: &ME9_P1,
            device: "",
            verbose: true,
        };
        let args = strings(cli.dry_run(Path::new("t.bin"), Some(Path::new("prepared.bin"))));
        assert!(!args.contains(&"-d".to_string()));
        assert_eq!(&args[..2], ["--json", "-v"]);
        let flash = args.iter().position(|a| a == "flash").unwrap();
        assert!(args.iter().position(|a| a == "-v").unwrap() < flash);
        assert_eq!(
            &args[flash..],
            [
                "flash",
                "-i",
                "t.bin",
                "--dry-run",
                "--emit-image",
                "prepared.bin"
            ]
        );

        let plain = strings(cli.dry_run(Path::new("t.bin"), None));
        assert_eq!(plain.last().map(String::as_str), Some("--dry-run"));
    }

    #[test]
    fn discovery_finds_the_cmake_build_tree_from_a_nested_dir() {
        let root = tempfile::tempdir().unwrap();
        let exe_dir = root.path().join("VolvoUI").join("target").join("debug");
        fs::create_dir_all(&exe_dir).unwrap();
        assert!(discover(Some(&exe_dir)).is_none());

        let built = root
            .path()
            .join("build")
            .join("VolvoFlasher")
            .join("Release");
        fs::create_dir_all(&built).unwrap();
        fs::write(built.join(EXE_NAME), b"").unwrap();
        assert_eq!(discover(Some(&exe_dir)), Some(built.join(EXE_NAME)));

        fs::write(exe_dir.join(EXE_NAME), b"").unwrap();
        assert_eq!(
            discover(Some(&exe_dir)),
            Some(exe_dir.join(EXE_NAME)),
            "a copy next to the app wins"
        );
    }

    #[test]
    fn override_beats_settings_and_blank_settings_are_ignored() {
        let configured = Some("C:\\tools\\VolvoFlasher.exe");
        let (path, source) = resolve(None, configured, None).unwrap();
        assert_eq!(
            (path, source),
            (
                PathBuf::from("C:\\tools\\VolvoFlasher.exe"),
                Source::Settings
            )
        );
        let forced = Path::new("D:\\sim\\fake_flasher.exe");
        let (path, source) = resolve(Some(forced), configured, None).unwrap();
        assert_eq!((path, source), (forced.to_path_buf(), Source::Env));
        assert!(resolve(None, Some("  "), None).is_none());
    }

    #[test]
    fn probe_reports_missing_files_without_running_anything() {
        let scratch = tempfile::tempdir().unwrap();
        let missing = probe(None, scratch.path(), &[]);
        assert!(!missing.flasher.found && missing.flasher.error.is_some());
        let gone = probe(
            Some((scratch.path().join("nope.exe"), Source::Settings)),
            scratch.path(),
            &[],
        );
        assert!(!gone.flasher.found && !gone.flasher.compatible);
    }
}

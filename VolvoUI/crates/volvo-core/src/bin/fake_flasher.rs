//! Stand-in for VolvoFlasher.exe that speaks the same `--json` protocol against a simulated ECU.
//!
//! The integration tests use it, and so can you to try the UI without hardware:
//! point `VOLVOUI_FLASHER` (or Connection > Flasher location) at the built `fake_flasher.exe`.
//!
//! The simulated ECU memory is a 2 MiB file that persists between runs, so a backup, a flash
//! and a verification read behave like the real thing. Environment variables shape the run:
//!
//! * `FAKE_ECU_IMAGE`    path of the simulated memory (default: temp dir, `volvoui-fake-ecu.bin`)
//! * `FAKE_DELAY_MS`     pause per progress step, to make jobs take visible time (default 0)
//! * `FAKE_CORRUPT_READ` flip one byte in the Nth read since the memory file was created
//! * `FAKE_FAIL`         `read`, `flash` (before erase), `erase` or `write`: fail at that stage
//! * `FAKE_CHECKSUM`     `valid` (default), `unsupported`, `correct` (a tuned file needs correcting,
//!   the stock image is fine), `reject-stock` (the routine rejects even the stock image) or
//!   `blind` (the check passes but evaluated no records, as on a table it does not understand)
//! * `FAKE_ERASE_GAP`    `1`: the erase also wipes 0x90000..0xA0000
//! * `FAKE_BAD_WRITE`    `1`: report success although one written byte is wrong
//! * `FAKE_PROTOCOL`     protocol number to announce (default: the real one)
//! * `FAKE_NO_DEVICES`   `1`: report no J2534 devices

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use volvo_core::protocol::{
    BootloaderPlan, ChecksumInfo, ChunkPlan, Device, Message, PROTOCOL_VERSION, Plan, Span, to_line,
};

const IMAGE_SIZE: usize = 0x20_0000;
const WRITE_CHUNKS: [(usize, usize); 2] = [(0x2_0000, 0x7_0000), (0xA_0000, 0x15_0000)];
const GAP: std::ops::Range<usize> = 0x9_0000..0xA_0000;
const SBL_ADDR: u64 = 0x7F_81D0;
const SBL_SIZE: u64 = 11_496;
const TICKS: u64 = 40;

fn emit(message: Message) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", to_line(&message));
    let _ = out.flush();
}

fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

fn pause() {
    let ms = var("FAKE_DELAY_MS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if ms > 0 {
        thread::sleep(Duration::from_millis(ms));
    }
}

fn state(name: &str) {
    emit(Message::State { state: name.into() });
    pause();
}

#[derive(Default)]
struct Args {
    json: bool,
    device: String,
    command: String,
    output: Option<PathBuf>,
    start: usize,
    size: usize,
    input: Option<PathBuf>,
    dry_run: bool,
    emit_image: Option<PathBuf>,
}

fn hex(value: Option<String>) -> usize {
    value
        .and_then(|v| usize::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0)
}

fn parse(argv: Vec<String>) -> Args {
    let mut args = Args::default();
    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => args.json = true,
            "-d" | "--device" => args.device = it.next().unwrap_or_default(),
            "-b" | "--baudrate" | "-f" | "--platform" | "-e" | "--ecu" | "-p" | "--pin"
            | "--cm-info" => {
                it.next();
            }
            "devices" | "read" | "flash" => args.command = arg,
            "-o" | "--output" => args.output = it.next().map(PathBuf::from),
            "-s" | "--start" => args.start = hex(it.next()),
            "-sz" | "--size" => args.size = hex(it.next()),
            "-i" | "--input" => args.input = it.next().map(PathBuf::from),
            "--dry-run" => args.dry_run = true,
            "--emit-image" => args.emit_image = it.next().map(PathBuf::from),
            _ => {}
        }
    }
    args
}

fn image_path() -> PathBuf {
    var("FAKE_ECU_IMAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("volvoui-fake-ecu.bin"))
}

fn generate_image() -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..IMAGE_SIZE)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn load_image(path: &Path) -> Result<Vec<u8>, String> {
    match fs::read(path) {
        Ok(data) if data.len() == IMAGE_SIZE => Ok(data),
        _ => {
            let data = generate_image();
            fs::write(path, &data).map_err(|e| {
                format!("Cannot create the simulated ECU at {}: {e}", path.display())
            })?;
            let _ = fs::remove_file(path.with_extension("reads"));
            Ok(data)
        }
    }
}

/// Counts reads since the memory file was created; returns the number of the current one.
fn bump_read_counter(image: &Path) -> u32 {
    let counter = image.with_extension("reads");
    let next = fs::read_to_string(&counter)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    let _ = fs::write(&counter, next.to_string());
    next
}

struct Meter {
    current: u64,
    max: u64,
}

impl Meter {
    fn new(max: u64) -> Self {
        Meter { current: 0, max }
    }

    fn advance(&mut self, total: u64) {
        for _ in 0..TICKS {
            self.current = (self.current + total / TICKS).min(self.max);
            emit(Message::Progress {
                current: self.current,
                max: self.max,
            });
            pause();
        }
    }
}

fn bootloader_stages(meter: &mut Meter) {
    state("FallAsleep");
    state("OpenChannels");
    state("LoadBootloader");
    meter.advance(SBL_SIZE);
    state("StartBootloader");
}

fn devices() -> Result<String, String> {
    let devices = if var("FAKE_NO_DEVICES").is_some() {
        Vec::new()
    } else {
        vec![Device {
            name: "Simulated J2534 device".into(),
            library: "fake_flasher".into(),
        }]
    };
    emit(Message::Devices { devices });
    Ok(String::new())
}

fn read(args: &Args) -> Result<String, String> {
    let output = args.output.clone().ok_or("Missing -o")?;
    let image_file = image_path();
    let image = load_image(&image_file)?;
    if args.size == 0 || args.start + args.size > IMAGE_SIZE {
        return Err(format!(
            "Read range {:X}+{:X} is outside the flash",
            args.start, args.size
        ));
    }

    let mut meter = Meter::new(SBL_SIZE + args.size as u64);
    bootloader_stages(&mut meter);
    state("EraseFlash");
    state("WriteFlash");
    if var("FAKE_FAIL").as_deref() == Some("read") {
        return Err("Simulated read failure".into());
    }
    meter.advance(args.size as u64);

    let mut data = image[args.start..args.start + args.size].to_vec();
    let nth = bump_read_counter(&image_file);
    if var("FAKE_CORRUPT_READ").and_then(|v| v.parse::<u32>().ok()) == Some(nth) {
        let at = data.len() / 3;
        data[at] ^= 0x5A;
    }
    fs::write(&output, data).map_err(|e| format!("Cannot write {}: {e}", output.display()))?;
    state("WakeUp");
    state("Done");
    Ok("Reading done".into())
}

/// What the flasher would send for `input`, and the image it would send (0xFF outside the chunks).
fn prepare(input: &Path) -> Result<(Plan, Vec<u8>), String> {
    let data = fs::read(input).map_err(|e| format!("Cannot open {}: {e}", input.display()))?;
    if data.len() < 0x1F_0000 {
        return Err("Flash file too small".into());
    }
    let stock = load_image(&image_path())? == data;
    let mode = var("FAKE_CHECKSUM").unwrap_or_else(|| "valid".into());
    // (supported, valid, bytes it would correct, records it really checked)
    let (supported, valid, modified, checked_blocks) = match (mode.as_str(), stock) {
        ("unsupported", _) => (false, false, 0u64, 0u64),
        ("blind", _) => (true, true, 0, 0),
        ("reject-stock", _) | ("correct", false) => (true, false, 4, 60),
        _ => (true, true, 0, 60),
    };

    let mut prepared = vec![0xFF; data.len()];
    for (start, length) in WRITE_CHUNKS {
        prepared[start..start + length].copy_from_slice(&data[start..start + length]);
    }
    for i in 0..modified as usize {
        prepared[0xA_0010 + i] ^= 0xFF;
    }

    let plan = Plan {
        platform: "P1".into(),
        ecu: "7A".into(),
        cm_info: "me9_p1".into(),
        file_size: data.len() as u64,
        checksum: ChecksumInfo {
            supported,
            valid,
            checked_blocks,
        },
        bootloader: BootloaderPlan {
            call: SBL_ADDR,
            chunks: vec![Span {
                start: SBL_ADDR,
                length: SBL_SIZE,
            }],
        },
        erase: vec![Span {
            start: 0x2_0000,
            length: 0,
        }],
        chunks: WRITE_CHUNKS
            .iter()
            .enumerate()
            .map(|(i, &(start, length))| ChunkPlan {
                start: start as u64,
                length: length as u64,
                modified: if i == 1 { modified } else { 0 },
            })
            .collect(),
    };
    Ok((plan, prepared))
}

fn flash(args: &Args) -> Result<String, String> {
    let input = args.input.clone().ok_or("Missing -i")?;
    let (plan, prepared) = prepare(&input)?;
    if args.dry_run {
        emit(Message::Plan(plan));
        if let Some(path) = &args.emit_image {
            fs::write(path, &prepared)
                .map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
        }
        return Ok("Dry run".into());
    }

    let image_file = image_path();
    let mut image = load_image(&image_file)?;
    let fail = var("FAKE_FAIL");
    let total: u64 = WRITE_CHUNKS.iter().map(|&(_, length)| length as u64).sum();
    let mut meter = Meter::new(SBL_SIZE + total * 2);
    bootloader_stages(&mut meter);
    if fail.as_deref() == Some("flash") {
        return Err("Simulated failure before the erase".into());
    }

    state("EraseFlash");
    for (start, length) in WRITE_CHUNKS {
        image[start..start + length].fill(0xFF);
    }
    if var("FAKE_ERASE_GAP").as_deref() == Some("1") {
        image[GAP].fill(0xFF);
    }
    fs::write(&image_file, &image).map_err(|e| e.to_string())?;
    meter.advance(total);
    if fail.as_deref() == Some("erase") {
        return Err("Simulated erase failure".into());
    }

    state("WriteFlash");
    for (start, length) in WRITE_CHUNKS {
        image[start..start + length].copy_from_slice(&prepared[start..start + length]);
        fs::write(&image_file, &image).map_err(|e| e.to_string())?;
        meter.advance(length as u64);
        if fail.as_deref() == Some("write") {
            return Err("Simulated write failure".into());
        }
    }
    if var("FAKE_BAD_WRITE").is_some() {
        // The flasher reports success but one byte did not make it into the flash.
        image[0xB_0000] ^= 0x80;
        fs::write(&image_file, &image).map_err(|e| e.to_string())?;
    }
    state("WakeUp");
    state("Done");
    Ok("Flashing done".into())
}

fn main() -> ExitCode {
    let args = parse(env::args().skip(1).collect());
    if !args.json {
        eprintln!("fake_flasher only supports --json");
        return ExitCode::from(2);
    }
    let protocol = var("FAKE_PROTOCOL")
        .and_then(|v| v.parse().ok())
        .unwrap_or(PROTOCOL_VERSION);
    emit(Message::Hello {
        protocol,
        app: "VolvoFlasher".into(),
        version: "fake".into(),
        command: args.command.clone(),
    });
    // Commands that touch the car need a device, as in the real flasher.
    let needs_device = args.command == "read" || (args.command == "flash" && !args.dry_run);
    if needs_device && args.device.is_empty() {
        emit(Message::Result {
            ok: false,
            message: "No J2534 device selected".into(),
        });
        return ExitCode::from(1);
    }
    let result = match args.command.as_str() {
        "devices" => devices(),
        "read" => read(&args),
        "flash" => flash(&args),
        other => Err(format!("Unknown command {other:?}")),
    };
    match result {
        Ok(message) => {
            emit(Message::Result { ok: true, message });
            ExitCode::SUCCESS
        }
        Err(message) => {
            emit(Message::Result { ok: false, message });
            ExitCode::from(1)
        }
    }
}

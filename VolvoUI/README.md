# VolvoTools UI

A desktop app for backing up and flashing the engine ECU of a Volvo P1 car (Bosch ME9: S40, V50,
C30, C70 era). React front end, Rust back end, [Tauri](https://tauri.app) shell.

The app does not talk to the car itself. It drives `VolvoFlasher.exe` from this repository and adds
what a GUI needs around it: a backup library, a check of every file against your own backup, and
safety gates. The design is in [`docs/tech_specs/ui_json_protocol.md`](../docs/tech_specs/ui_json_protocol.md).

## What it does

1. **Connection**: finds `VolvoFlasher.exe`, lists the J2534 adapters installed on the PC.
2. **Backup**: reads the whole ECU **twice** and keeps the copy as *verified* only if both reads
   match byte for byte.
3. **Flash**: compares your tuned file with the verified backup (a map of what differs, by 64 KB
   sector), asks the flasher what it would write, runs the checklist, flashes, then reads the ECU
   back and compares it with the file.
4. **Activity**: the log of every command, the flasher output, and a history of finished jobs.

Flashing an ECU can leave the car undrivable until it is flashed again. Keep a copy of the original
backup somewhere off this PC, use a battery charger, and do not interrupt a flash.

## What stops you from making a mistake

All of these are enforced in the Rust back end, not only in the UI:

* A flash can only start from a backup that two reads confirmed, and the backup file is re-hashed
  first.
* The file must be exactly 2 MB and differ from the backup.
* The flasher's own write plan must equal the expected layout. The first 128 KB are never written.
* If your file has stale checksums, the flasher may correct them only if its checksum routine has
  first been shown to understand **your own stock backup**. You also confirm how many bytes change.
  A check that evaluated nothing is reported as "found no checksum table", never as "valid".
* You tick a checklist and type `FLASH`.
* After the flash the written areas are read back and compared with the exact image that was sent.
* While the ECU is being erased or written, a plain Stop is refused. A separate Force stop exists.
* The window cannot be closed during a job.

## Requirements

* Windows 10 or 11 with the WebView2 runtime (included in Windows 11).
* [Node.js](https://nodejs.org) 20 or newer.
* [Rust](https://rustup.rs) (stable, MSVC toolchain) and the Visual Studio C++ build tools.
* A built `VolvoFlasher.exe` that supports `--json`, see below.

## Build VolvoFlasher

Use the steps in the repository's `AGENTS.md`. J2534 drivers are usually 32-bit, and a 64-bit
VolvoFlasher cannot load them, so for a real adapter build the x86 flavour, as the CI does:

```powershell
git submodule update --init
conan install . --build=missing -s arch=x86 -s compiler.cppstd=17
cmake --preset conan-default -A Win32
cmake --build build --config Release --target VolvoFlasher
```

The app finds `build\VolvoFlasher\Release\VolvoFlasher.exe` by itself when it runs from this
repository. Otherwise choose the file under **Connection**. An older VolvoFlasher without `--json`
is detected and reported.

## Run

```powershell
cd VolvoUI
npm install
npm run tauri dev
```

### Try it without a car

```powershell
npm run dev:sim
```

This runs the app against `fake_flasher`, a simulated flasher with a fake 2 MB ECU. Backups, flashes
and read-backs behave like the real thing, and everything stays in `target\sim-home`, apart from
your real settings and backups. The app shows a "Simulator" banner. Failures can be injected with
environment variables, see the header of `crates/volvo-core/src/bin/fake_flasher.rs`, for example:

```powershell
$env:FAKE_CORRUPT_READ = "2"   # the second read comes back with one wrong byte
$env:FAKE_BAD_WRITE = "1"      # the flasher says it succeeded but one byte is wrong
$env:FAKE_CHECKSUM = "correct" # the tuned file needs its checksums corrected
npm run dev:sim
```

## Tests

```powershell
npm test                                 # front end logic
cargo test --workspace                   # back end, including full workflows on the simulator
npx tsc --noEmit                         # types
cargo clippy --workspace --all-targets   # lints
```

Contract tests against a real `VolvoFlasher.exe` need neither a car nor an adapter:

```powershell
$env:VOLVOUI_FLASHER_EXE = "C:\path\to\VolvoFlasher.exe"
cargo test -p volvo-core --test real_flasher -- --ignored
```

## Installer

```powershell
npm run tauri build
```

builds an NSIS installer under `target\release\bundle`. The icons are Tauri's defaults; replace
them with `npx tauri icon <1024px png>`.

## Where things are

| What | Where |
|---|---|
| Settings | `%APPDATA%\com.volvotools.ui\settings.json` |
| Backups (default, changeable) | `Documents\VolvoTools\Backups\<date>_p1-me9\ecu.bin` + `meta.json` |
| Flasher logs and job history | `%LOCALAPPDATA%\com.volvotools.ui\runs`, `history.jsonl` |
| Everything, when `VOLVOUI_HOME` is set | that folder |

`VOLVOUI_FLASHER` overrides the flasher path.

## Layout

```
src/                       React app: views, components, state, tests of the pure logic
src-tauri/                 Tauri shell: window, permissions, thin commands
crates/volvo-core/         Rust back end: protocol, memory map, backup library, safety gates, jobs
  src/bin/fake_flasher.rs  the simulator
  tests/                   workflow tests (simulator) and contract tests (real flasher)
scripts/dev-sim.mjs        npm run dev:sim
```

## Limits

* Only the ME9 engine ECU of P1 is supported. The memory map is in
  `crates/volvo-core/src/profile.rs`.
* The flasher's erase may also clear the 64 KB gap at `0x090000`. The read-back after a flash checks
  this and says so.
* The checksum routine has not been validated against an ME9 image in this repository. The check on
  your own stock backup is the safeguard; if it says nothing was checked, correct the checksums in
  your tuning software.

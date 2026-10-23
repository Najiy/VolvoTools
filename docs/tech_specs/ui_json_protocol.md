# Spec: VolvoTools UI and the `--json` mode of VolvoFlasher

## 1. Goal

Give VolvoTools a desktop UI (React front end, Rust back end) for the job most owners actually do:
back up an engine ECU, flash a tuned file, and check the result. The first supported target is the
Bosch ME9 engine ECU of the Volvo P1 platform (S40, V50, C30, C70 era).

The UI must not contain a second implementation of the ECU protocol. `VolvoFlasher.exe` stays the
only program that talks to the car. The UI drives it through a small, versioned, machine-readable
interface and adds the safety checks a GUI needs around it.

## 2. Architecture

```
React (TypeScript)            VolvoUI/src
   │  invoke() / events
Tauri commands                VolvoUI/src-tauri        thin: no decisions
   │
volvo-core (Rust)             VolvoUI/crates/volvo-core
   │  backup library, memory map, diff, safety gates, job control
   │  spawns the process, reads JSON lines from stdout
VolvoFlasher.exe --json       VolvoFlasher/src/VolvoFlasher.cpp
   │  Flasher / Common libraries (D2 protocol, SBL, checksums)
J2534 adapter ── CAN HS ── ECU
```

`volvo-core` has no GUI dependency. Its tests run in a second or two, and `fake_flasher`
(section 8) lets the whole workflow run without a car.

## 3. JSON protocol, version 1

Enabled by the global option `--json`. Each structured line on stdout is one JSON object with a
`type` member. The reader must ignore unknown types and unknown members, and must treat every
line that is not a JSON object as plain log text: easylogging output (with `-v`) and legacy text
share stdout.

| `type` | Members | When |
|---|---|---|
| `hello` | `protocol` (number), `app`, `version`, `command` | First line after the arguments are parsed |
| `devices` | `devices`: `[{ name, library }]` | `devices` command |
| `state` | `state`: a `FlasherState` name | Every flasher state change |
| `progress` | `current`, `max` (bytes) | About every 250 ms while the value changes, and once at the end |
| `plan` | see below | `flash --dry-run` |
| `result` | `ok`, `message` | Always the last line, exactly once |

Exit code is 0 exactly when `result.ok` is true.

`state` names are the enumerators of `flasher::FlasherState`: `FallAsleep`, `OpenChannels`,
`LoadBootloader`, `StartBootloader`, `EraseFlash`, `WriteFlash`, `WakeUp`, `Done`, `Error` and so
on. A read goes through the same sequence as a write: the D2 reader reports `EraseFlash` (a
no-op) and `WriteFlash` while it is reading. Consumers must use the job they started, not the
state name, to decide what is happening.

### 3.1 The `plan` message

```json
{"type":"plan","platform":"P1","ecu":"7A","cmInfo":"me9_p1","fileSize":2097152,
 "checksum":{"supported":true,"valid":true,"checkedBlocks":60},
 "bootloader":{"call":8356304,"chunks":[{"start":8356304,"length":11496}]},
 "erase":[{"start":131072,"length":0}],
 "chunks":[{"start":131072,"length":458752,"modified":0},
           {"start":655360,"length":1376256,"modified":0}]}
```

* `checksum.supported`: `util::ChecksumHelper` knows a scheme for this ECU and image size.
* `checksum.valid`: the file as given already satisfies that scheme.
* `checksum.checkedBlocks`: how many checksum records the check really evaluated
  (`ChecksumHelper::countCheckedBlocks`). **`valid` alone is not evidence.** The ME7-style check
  returns true as soon as a record points outside the image, so a file whose table is not in the
  expected format "passes" without anything being verified; that case reports `checkedBlocks` 0.
  A file with a correct 60-record table reports 60. A damaged one reports at least 1 and
  `valid` false, because the check stops at the first mismatch.
* `chunks[].modified`: how many bytes of the chunk differ from the file because
  `loadVBFForFlasher` corrected checksums. Only meaningful for flat `.bin` input.
* `erase[].length` of 0 means "from `start`, as the bootloader defines it".

`plan` is computed by the same code path as a real flash (`loadVBFForFlasher` and
`SBLProviderCommon::getSBL`), so it shows what would run.

### 3.2 Versioning

`hello.protocol` is 1. A reader that sees another number must refuse to run jobs. A flasher build
without `--json` fails argument parsing and never prints `hello`, which the UI reports as "this
build does not support the UI protocol".

## 4. Command line changes

| Option | Meaning |
|---|---|
| `--json` | Machine-readable output as described above. Requires `-d` for hardware commands; the named adapter (exact name first, then substring) is the only one used. |
| `--cm-info <variant>` | Software variant, for example `me9_p1`. Selects the bootloader and flash layout. For `-f P1 -e 7A` it defaults to `me9_p1`; before, P1 silently used the ME7 bootloader. |
| `devices` | Lists registered J2534 devices. |
| `flash --dry-run` | With `--json`: print `plan` and exit. No device is opened. |
| `flash --emit-image <file>` | With `--dry-run`: write the exact image that would be sent (0xFF outside the written chunks). |

`flash` with `--cm-info` now flashes through `loadVBFForFlasher`, so a P1 ME9 `.bin` is split into
the two Volvo blocks and the correct bootloader (`0x7F81D0`) is used. Without `--cm-info` the old
P2 behaviour is unchanged. `flash` refuses to run when the flash file or the bootloader is empty;
without a bootloader the erase and write commands would reach the primary bootloader.

`read` honours `--cm-info` as well, and now fails when the number of bytes returned differs from
the number requested.

Also fixed on the way: `-b <rate>` always failed with `Bad any_cast`. The option scans an
`unsigned long` but was declared with an `unsigned` default and read back as `unsigned`, so only
the default worked. Both now use `unsigned long`.

## 5. Target: ME9 on P1

`VolvoUI/crates/volvo-core/src/profile.rs` mirrors `createVBFForME9P1`:

| Area | Range | Flasher |
|---|---|---|
| Low block | `0x000000` to `0x01FFFF` | never writes |
| Block A | `0x020000` to `0x08FFFF` | erases and writes |
| Gap | `0x090000` to `0x09FFFF` | never writes |
| Block B | `0x0A0000` to `0x1EFFFF` | erases and writes |
| Tail | `0x1F0000` to `0x1FFFFF` | never writes |

The UI refuses a flash whose reported plan differs from this layout.

## 6. Safety gates

All gates are enforced in `volvo-core`, not in the front end, so a UI bug cannot skip one.

1. A flash starts only from a **verified backup**: two reads that matched byte for byte. The
   backup image is re-hashed against the value recorded when it was made.
2. The file must be exactly 2 MiB and must differ from the backup.
3. The flasher's own `plan` must equal the expected erase and write layout.
4. **Checksum self-test.** The flasher's checksum routine (`ChecksumHelper`, an ME7-style table
   also used for 2 MiB images) is run on the user's stock backup. The routine counts as tested
   only if it evaluated at least one record (`checkedBlocks` above 0) and accepted the image. If
   it would have to change the *stock* image, or found nothing to check in it, automatic
   correction is blocked. If the routine passed on the stock image but wants to change the tuned
   file, the user must acknowledge the number of bytes first. A file the routine could not read
   at all is flashed as given, with a warning that nothing was checked.
5. After a flash the written areas are read back and compared with the image the flasher
   prepared (`--emit-image`), so corrected checksums do not cause a false alarm.
6. The user must tick every item of a checklist and type `FLASH`; both are checked again in the
   back end.
7. A plain stop is refused while the ECU is being erased or written. A separate force stop exists
   for a stuck flasher.
8. Closing the window is blocked while a job runs.

## 7. Known limits

* It is not known from the repository whether the bootloader's erase command, issued at
  `0x20000` with length 0, also clears the gap (`0x090000` to `0x09FFFF`). The read-back after a
  flash compares the gap with the backup and reports a change.
* The checksum routine has not been validated on an ME9 image from this repository. Gate 4 is the
  runtime substitute for that.
* A read cannot detect a late reply that shifts data. Reading twice and comparing is the guard.
* J2534 drivers are usually 32-bit. A 64-bit VolvoFlasher cannot load them, so the flasher used
  with real adapters is normally the `x86` build.

## 8. Simulator

`fake_flasher` (`crates/volvo-core/src/bin/fake_flasher.rs`) speaks the same protocol against a
2 MiB memory file. It keeps state between runs, so a backup, a flash and a read-back behave like the
real thing, and environment variables inject faults (a corrupted read, a failed erase, a silent bad
write, a checksum routine that rejects the stock image). `npm run dev:sim` runs the UI against it with
all state in `VolvoUI/target/sim-home`, apart from the real settings and backups.

## 9. Tests

* `cargo test --workspace`: unit tests for protocol parsing, memory map, diff, library and job
  control, plus integration tests of the full workflows against the simulator.
* Contract tests against the real `VolvoFlasher.exe`, ignored by default. They need no car and
  no adapter (`devices` and `flash --dry-run` only):

  ```text
  set VOLVOUI_FLASHER_EXE=C:\path\to\VolvoFlasher.exe
  cargo test -p volvo-core --test real_flasher -- --ignored
  ```

  They build images with a correct, a stale and an unreadable checksum table, and check that the
  verdicts and gates come out as described in section 6.
* `npm test`: unit tests for formatting, time estimates, job state and stage mapping.
* The C++ side has no unit tests of its own in this repository.

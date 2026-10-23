// Runs the app against the simulated flasher so the whole UI can be tried without a car.
//
//   npm run dev:sim
//
// Settings, backups and the fake 2 MB ECU live in target/sim-home. Delete that folder to start over.
// See crates/volvo-core/src/bin/fake_flasher.rs for the FAKE_* variables that inject failures.

import { spawn, spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const shell = process.platform === "win32";

const build = spawnSync("cargo", ["build", "-p", "volvo-core", "--bin", "fake_flasher"], {
  cwd: root,
  stdio: "inherit",
  shell,
});
if (build.status !== 0) process.exit(build.status ?? 1);

const fake = resolve(root, "target", "debug", shell ? "fake_flasher.exe" : "fake_flasher");
// Everything the simulator creates stays here, apart from your real settings and backups.
const home = resolve(root, "target", "sim-home");
const env = {
  ...process.env,
  VOLVOUI_FLASHER: fake,
  VOLVOUI_HOME: home,
  FAKE_ECU_IMAGE: resolve(home, "fake-ecu.bin"),
  // Slow enough to watch, fast enough not to bore: a read takes about a minute.
  FAKE_DELAY_MS: process.env.FAKE_DELAY_MS ?? "40",
};
console.log(`Using the simulator at ${fake}`);

const app = spawn("npx", ["tauri", "dev"], { cwd: root, stdio: "inherit", env, shell });
app.on("exit", (code) => process.exit(code ?? 0));

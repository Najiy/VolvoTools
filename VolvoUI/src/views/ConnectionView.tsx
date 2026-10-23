import { open } from "@tauri-apps/plugin-dialog";
import { useState } from "react";
import { api, errorText } from "../api";
import { Banner } from "../components/Banner";
import { Button } from "../components/Button";
import { Toggle } from "../components/Toggle";
import { useApp } from "../state/AppState";
import type { FlasherStatus } from "../types";

const SOURCE_TEXT: Record<NonNullable<FlasherStatus["source"]>, string> = {
  env: "Set by the VOLVOUI_FLASHER environment variable",
  settings: "Chosen by you",
  auto: "Found automatically",
};

export function ConnectionView() {
  const { info, settings, scan, scanning, rescan, updateSettings, refreshBackups, device, job } = useApp();
  const [error, setError] = useState<string | null>(null);
  if (!info || !settings) return null;

  const flasher = scan?.flasher;
  const devices = scan?.devices ?? [];
  const backupsDir = settings.backupsDir ?? info.defaultBackupsDir;

  async function attempt(work: () => Promise<void>) {
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(errorText(e));
    }
  }

  const chooseFlasher = () =>
    attempt(async () => {
      const picked = await open({ multiple: false, directory: false, filters: [{ name: "Program", extensions: ["exe"] }] });
      if (typeof picked === "string") {
        await updateSettings({ flasherPath: picked });
        await rescan();
      }
    });

  const chooseBackups = () =>
    attempt(async () => {
      const picked = await open({ multiple: false, directory: true });
      if (typeof picked === "string") {
        await updateSettings({ backupsDir: picked });
        await refreshBackups();
      }
    });

  return (
    <>
      <header className="view-head">
        <h1>Connection</h1>
        <p className="lead">Choose your J2534 adapter and check that the flasher program is ready.</p>
      </header>

      {error && <Banner tone="danger">{error}</Banner>}

      <section className="section" aria-labelledby="flasher-title">
        <div className="section-head">
          <h2 id="flasher-title">Flasher program</h2>
        </div>
        {!scan ? (
          <p className="muted">Looking for VolvoFlasher.exe</p>
        ) : flasher?.compatible ? (
          <div className="stack">
            <div className="row">
              <span className="dot" data-tone="ok" />
              <strong>Ready</strong>
              {flasher.version && <span className="muted">version {flasher.version}</span>}
            </div>
            <p className="mono muted">{flasher.path}</p>
            {flasher.source && <p className="hint">{SOURCE_TEXT[flasher.source]}</p>}
          </div>
        ) : (
          <Banner tone="warn" title="The flasher program is not ready">
            <p>{flasher?.error ?? "VolvoFlasher.exe was not found."}</p>
            {flasher?.path && <p className="mono hint">{flasher.path}</p>}
            <p className="hint">
              Build it from this repository (see VolvoUI/README.md), then choose VolvoFlasher.exe below.
            </p>
          </Banner>
        )}
        <div className="row">
          <Button onClick={chooseFlasher} disabled={job.running}>
            Choose file
          </Button>
          {settings.flasherPath && (
            <Button
              variant="quiet"
              disabled={job.running}
              onClick={() => attempt(async () => (await updateSettings({ flasherPath: null }), await rescan()))}
            >
              Use automatic location
            </Button>
          )}
          <Button variant="quiet" icon="refresh" busy={scanning} onClick={() => attempt(rescan)}>
            Rescan
          </Button>
        </div>
      </section>

      <section className="section" aria-labelledby="adapter-title">
        <div className="section-head">
          <h2 id="adapter-title">J2534 adapter</h2>
        </div>
        {devices.length === 0 ? (
          <div className="empty">
            <strong>{flasher?.compatible ? "No adapters found" : "Adapters appear once the flasher program is ready"}</strong>
            {flasher?.compatible && (
              <p>
                J2534 drivers register themselves in Windows when you install them. Install your adapter&apos;s
                driver, plug the adapter in, then rescan. 32-bit adapters need a 32-bit VolvoFlasher build.
              </p>
            )}
          </div>
        ) : (
          <div className="choice-list" role="radiogroup" aria-label="J2534 adapter">
            {devices.map((d) => (
              <label key={d.name} className="choice-row" data-selected={d.name === device?.name}>
                <input
                  type="radio"
                  name="device"
                  checked={d.name === device?.name}
                  disabled={job.running}
                  onChange={() => attempt(() => updateSettings({ device: d.name }))}
                />
                <span>
                  {d.name}
                  <small className="mono">{d.library}</small>
                </span>
              </label>
            ))}
          </div>
        )}
      </section>

      <section className="section" aria-labelledby="target-title">
        <div className="section-head">
          <h2 id="target-title">Target</h2>
        </div>
        <dl className="facts">
          <dt>Car</dt>
          <dd>{info.profile.title}</dd>
          <dt>Control module</dt>
          <dd>Engine control module, address {info.profile.ecu}</dd>
          <dt>Network</dt>
          <dd>CAN high speed, {info.profile.baudrate / 1000} kbit/s (OBD pins 6 and 14)</dd>
          <dt>Flash size</dt>
          <dd>{(info.profile.imageSize / (1024 * 1024)).toFixed(0)} MB</dd>
        </dl>
      </section>

      <section className="section" aria-labelledby="options-title">
        <div className="section-head">
          <h2 id="options-title">Safety options</h2>
        </div>
        <div className="stack">
          <Toggle
            checked={settings.verifyReads}
            disabled={job.running}
            onChange={(verifyReads) => attempt(() => updateSettings({ verifyReads }))}
            label="Read twice and compare when making a backup"
            hint="A backup only counts as verified when both reads match byte for byte. Turn this off and the copy cannot be used as the base for a flash."
          />
          <Toggle
            checked={settings.verifyAfterFlash}
            disabled={job.running}
            onChange={(verifyAfterFlash) => attempt(() => updateSettings({ verifyAfterFlash }))}
            label="Read back and compare after flashing"
            hint="Adds a second transfer after the flash and checks the ECU against your file."
          />
          <Toggle
            checked={settings.verbose}
            disabled={job.running}
            onChange={(verbose) => attempt(() => updateSettings({ verbose }))}
            label="Include the flasher library log in the activity view"
            hint="Useful when reporting a problem. Adds a lot of lines."
          />
        </div>
      </section>

      <section className="section" aria-labelledby="files-title">
        <div className="section-head">
          <h2 id="files-title">Files</h2>
        </div>
        <dl className="facts">
          <dt>Backups</dt>
          <dd className="stack">
            <span className="mono">{backupsDir}</span>
            <span className="row">
              <Button size="sm" icon="folder" onClick={() => attempt(api.revealBackups)}>
                Open
              </Button>
              <Button size="sm" disabled={job.running} onClick={chooseBackups}>
                Change
              </Button>
              {settings.backupsDir && (
                <Button
                  size="sm"
                  variant="quiet"
                  disabled={job.running}
                  onClick={() => attempt(async () => (await updateSettings({ backupsDir: null }), await refreshBackups()))}
                >
                  Use default
                </Button>
              )}
            </span>
          </dd>
          <dt>Logs</dt>
          <dd className="stack">
            <span className="mono">{info.logDir}</span>
            <span className="row">
              <Button size="sm" icon="folder" onClick={() => attempt(api.revealLogs)}>
                Open
              </Button>
            </span>
          </dd>
        </dl>
      </section>
    </>
  );
}

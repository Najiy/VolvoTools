import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import { api, errorText } from "../api";
import { Banner } from "../components/Banner";
import { Button } from "../components/Button";
import { Icon } from "../components/Icon";
import { JobPanel } from "../components/JobPanel";
import { Outcome } from "../components/Outcome";
import { SectorStrip } from "../components/SectorStrip";
import { StopControl } from "../components/StopControl";
import { Toggle } from "../components/Toggle";
import { formatBytes, formatCount, formatWhen, hex, plural, shortHash } from "../lib/format";
import { useApp } from "../state/AppState";
import type { AppInfo, ChecksumVerdict, FlashPreview } from "../types";
import type { View } from "../App";

const PREFLIGHT_TEXT: Record<string, string> = {
  charger: "A battery charger or stable power supply is connected.",
  ignition: "The ignition is on and the engine is off.",
  loads: "Lights, climate control, radio and other loads are switched off.",
  laptop: "This laptop is on mains power and will not go to sleep.",
  cable: "The adapter and its cables are firmly connected and will not be touched.",
};

type CheckTone = "ok" | "warn" | "danger" | "idle";

interface CheckItem {
  tone: CheckTone;
  text: string;
}

function checkItems(preview: FlashPreview, info: AppInfo): CheckItem[] {
  const items: CheckItem[] = [];
  const sizeOk = preview.file.size === info.profile.imageSize;
  items.push({
    tone: sizeOk ? "ok" : "danger",
    text: sizeOk
      ? `The file is ${formatCount(preview.file.size)} bytes, a full image`
      : `The file is ${formatCount(preview.file.size)} bytes, a full image is ${formatCount(info.profile.imageSize)}`,
  });

  const tune: ChecksumVerdict | null = preview.tuneChecksum;
  if (!tune) items.push({ tone: "idle", text: "Checksums have not been checked yet" });
  else if (tune.status === "valid")
    items.push({ tone: "ok", text: `Checksums in the file are valid (${plural(tune.checkedBlocks, "record")} checked)` });
  else if (tune.status === "correctable")
    items.push({ tone: "warn", text: `The flasher would change ${plural(tune.modifiedBytes, "byte")} to correct checksums` });
  else if (tune.status === "unsupported")
    items.push({ tone: "warn", text: "The flasher found no checksum table it understands in this file" });
  else items.push({ tone: "danger", text: `The flasher could not check the file${tune.detail ? `: ${tune.detail}` : ""}` });

  const stock = preview.backupChecksum;
  if (stock?.status === "valid")
    items.push({
      tone: "ok",
      text: `The flasher's checksum routine accepts your stock backup (${plural(stock.checkedBlocks, "record")} checked)`,
    });
  else if (stock?.status === "correctable")
    items.push({ tone: "danger", text: "The flasher's checksum routine rejects your own stock backup" });
  else if (stock?.status === "unsupported")
    items.push({
      tone: "warn",
      text: "The flasher's checksum routine found nothing to check in your stock backup, so it will not correct checksums",
    });
  else if (stock?.status === "unavailable")
    items.push({ tone: "warn", text: "The checksum routine could not be tested on your backup" });
  return items;
}

interface ReviewProps {
  preview: FlashPreview;
  info: AppInfo;
  acknowledged: boolean;
  onAcknowledge: (value: boolean) => void;
}

function Review({ preview, info, acknowledged, onAcknowledge }: ReviewProps) {
  const diff = preview.diff;
  return (
    <div className="stack">
      {diff && (
        <>
          <p>
            {diff.identical
              ? "The file is identical to your backup."
              : `${plural(diff.totalChanged, "byte")} differ from your backup, ${formatCount(diff.changedInWritten)} of them in areas the flasher writes.`}
          </p>
          <SectorStrip
            imageSize={info.profile.imageSize}
            sectorSize={info.sectorSize}
            regions={info.profile.regions}
            changed={diff.sectors}
            label="Comparison of your file with the backup, by 64 KB sector"
            showLegend
          />
          <table className="table">
            <thead>
              <tr>
                <th>Area</th>
                <th>Range</th>
                <th>Flasher</th>
                <th className="num">Differs</th>
              </tr>
            </thead>
            <tbody>
              {diff.regions.map((region) => (
                <tr key={region.name}>
                  <td>{region.name}</td>
                  <td className="mono">
                    {hex(region.start)} to {hex(region.end - 1)}
                  </td>
                  <td>{region.role === "written" ? "Writes" : "Never writes"}</td>
                  <td className="num">
                    {region.changed === 0 ? (
                      <span className="faint">none</span>
                    ) : region.role === "notWritten" ? (
                      <span className="pill" data-tone="warn" title="These changes will not be applied">
                        {formatCount(region.changed)} ignored
                      </span>
                    ) : (
                      formatCount(region.changed)
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      <ul className="checks">
        {checkItems(preview, info).map((item) => (
          <li key={item.text} data-tone={item.tone}>
            <Icon name={item.tone === "ok" ? "check" : item.tone === "idle" ? "info" : item.tone === "warn" ? "warn" : "cross"} />
            <span>{item.text}</span>
          </li>
        ))}
      </ul>

      {preview.blockers.length > 0 && (
        <Banner tone="danger" title="This file cannot be flashed yet">
          {preview.blockers.map((text) => (
            <p key={text}>{text}</p>
          ))}
        </Banner>
      )}
      {preview.warnings.length > 0 && (
        <Banner tone="warn" title="Worth knowing">
          {preview.warnings.map((text) => (
            <p key={text}>{text}</p>
          ))}
        </Banner>
      )}
      {preview.requiresChecksumAck && preview.canFlash && (
        <label className="check">
          <input type="checkbox" checked={acknowledged} onChange={(e) => onAcknowledge(e.target.checked)} />
          <span>
            Let the flasher change {plural(preview.tuneChecksum?.modifiedBytes ?? 0, "byte")} to correct the checksums. Your
            stock backup passes the same routine. The read-back is compared with the corrected data.
          </span>
        </label>
      )}
    </div>
  );
}

export function FlashView({ go }: { go: (view: View) => void }) {
  const { info, settings, job, draft, setDraft, verifiedBackups, blockedReason, scan, startFlash, cancel, updateSettings } =
    useApp();
  const [preview, setPreview] = useState<FlashPreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [ticked, setTicked] = useState<string[]>([]);
  const [confirm, setConfirm] = useState("");
  const [startError, setStartError] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState<number | null>(null);

  const tunePath = draft.tunePath.trim().replace(/^"(.*)"$/, "$1");
  const backupId = draft.backupId;
  const flasherPath = scan?.flasher.path ?? null;

  // Default to the newest verified backup, and drop a choice that no longer exists.
  useEffect(() => {
    const stillThere = verifiedBackups.some((b) => b.id === backupId);
    if (!stillThere) setDraft({ backupId: verifiedBackups[0]?.id ?? null });
  }, [verifiedBackups, backupId, setDraft]);

  // Ask the backend to assess the file whenever the inputs change.
  useEffect(() => {
    if (!tunePath || !backupId) {
      setPreview(null);
      setPreviewError(null);
      setPreviewing(false);
      return;
    }
    let stale = false;
    setPreviewing(true);
    const timer = window.setTimeout(async () => {
      try {
        const result = await api.previewFlash(tunePath, backupId);
        if (!stale) {
          setPreview(result);
          setPreviewError(null);
        }
      } catch (e) {
        if (!stale) {
          setPreview(null);
          setPreviewError(errorText(e));
        }
      } finally {
        if (!stale) setPreviewing(false);
      }
    }, 350);
    return () => {
      stale = true;
      window.clearTimeout(timer);
    };
  }, [tunePath, backupId, flasherPath]);

  // Anything the user confirmed applies to one specific file and backup. Start over if they change.
  useEffect(() => {
    setTicked([]);
    setConfirm("");
    setAcknowledged(false);
    setStartError(null);
  }, [tunePath, backupId, preview?.file.sha256]);

  if (!info || !settings) return null;

  const flashJob = job.job?.kind === "flash";
  const showPanel = job.running && flashJob;
  const showOutcome = !job.running && flashJob && job.outcome !== null && dismissed !== job.job?.id;
  const hasInputs = tunePath !== "" && backupId !== null;
  const checklistDone = info.preflightItems.every((id) => ticked.includes(id));
  const confirmed = confirm.trim() === info.confirmPhrase;
  const needsAck = preview?.requiresChecksumAck ?? false;

  const notReady =
    blockedReason ??
    (!preview?.canFlash
      ? "Resolve the problems above first."
      : needsAck && !acknowledged
        ? "Confirm the checksum correction above."
        : !checklistDone
          ? "Tick every item in the checklist."
          : !confirmed
            ? `Type ${info.confirmPhrase} to confirm.`
            : null);

  async function browse() {
    const picked = await open({ multiple: false, directory: false, filters: [{ name: "ECU image", extensions: ["bin"] }] });
    if (typeof picked === "string") setDraft({ tunePath: picked });
  }

  function tick(id: string) {
    setTicked((current) => (current.includes(id) ? current.filter((x) => x !== id) : [...current, id]));
  }

  async function flash() {
    if (!backupId) return;
    setStartError(null);
    try {
      await startFlash({
        tunePath,
        backupId,
        confirm,
        preflight: ticked,
        verifyAfter: settings?.verifyAfterFlash ?? true,
        allowChecksumChanges: acknowledged,
      });
    } catch (e) {
      setStartError(errorText(e));
    }
  }

  return (
    <>
      <header className="view-head">
        <h1>Flash</h1>
        <p className="lead">
          Write a tuned file to the engine ECU. The file is checked against your verified backup first, and the ECU
          is read back afterwards.
        </p>
      </header>

      {showPanel && <JobPanel job={job} info={info} stop={<StopControl cancel={cancel} />} />}

      {showOutcome && job.outcome && (
        <div className="stack">
          <Outcome outcome={job.outcome} kind="flash" onDismiss={() => setDismissed(job.job?.id ?? null)} />
          {job.outcome.ok && (
            <Banner tone="info">
              <p>The ECU now differs from your backup. Make a new backup of it before your next change.</p>
            </Banner>
          )}
        </div>
      )}

      {!showPanel && (
        <>
          <section className="section" aria-labelledby="flash-file-title">
            <div className="section-head">
              <span className="step-no">1</span>
              <h2 id="flash-file-title">Tuned file</h2>
            </div>
            <div className="field">
              <label className="sr-only" htmlFor="tune-path">
                Path of the tuned file
              </label>
              <div className="field-row">
                <input
                  id="tune-path"
                  className="input mono"
                  placeholder="Choose or paste the path of a full 2 MB .bin image"
                  value={draft.tunePath}
                  spellCheck={false}
                  onChange={(e) => setDraft({ tunePath: e.target.value })}
                />
                <Button icon="file" onClick={browse}>
                  Choose file
                </Button>
              </div>
              {preview && (
                <p className="hint">
                  {preview.file.name}, {formatBytes(preview.file.size)}, SHA-256{" "}
                  <span className="mono">{shortHash(preview.file.sha256, 16)}</span>
                </p>
              )}
            </div>
          </section>

          <section className="section" aria-labelledby="flash-base-title">
            <div className="section-head">
              <span className="step-no">2</span>
              <h2 id="flash-base-title">Base backup</h2>
            </div>
            {verifiedBackups.length === 0 ? (
              <div className="empty">
                <strong>No verified backup yet</strong>
                <p>A flash can only start from a backup that two matching reads have verified.</p>
                <div>
                  <Button icon="read" onClick={() => go("backup")}>
                    Make a backup
                  </Button>
                </div>
              </div>
            ) : (
              <div className="choice-list" role="radiogroup" aria-label="Base backup">
                {verifiedBackups.map((backup) => (
                  <label key={backup.id} className="choice-row" data-selected={backup.id === backupId}>
                    <input
                      type="radio"
                      name="backup"
                      checked={backup.id === backupId}
                      onChange={() => setDraft({ backupId: backup.id })}
                    />
                    <span>
                      {formatWhen(backup.createdAt).absolute}
                      <small>
                        <span className="mono">{shortHash(backup.sha256)}</span>, {formatWhen(backup.createdAt).relative}
                      </small>
                    </span>
                  </label>
                ))}
              </div>
            )}
          </section>

          <section className="section" aria-labelledby="flash-review-title" data-locked={!hasInputs}>
            <div className="section-head">
              <span className="step-no">3</span>
              <h2 id="flash-review-title">Review</h2>
            </div>
            {!hasInputs && <p className="section-note">Choose a file and a backup to compare.</p>}
            {hasInputs && previewing && !preview && <p className="muted">Checking the file with the flasher</p>}
            {previewError && <Banner tone="danger">{previewError}</Banner>}
            {preview && (
              <Review preview={preview} info={info} acknowledged={acknowledged} onAcknowledge={setAcknowledged} />
            )}
          </section>

          <section className="section" aria-labelledby="flash-arm-title" data-locked={!preview?.canFlash}>
            <div className="section-head">
              <span className="step-no">4</span>
              <h2 id="flash-arm-title">Check the car, then flash</h2>
            </div>
            <div className="stack">
              {info.preflightItems.map((id) => (
                <label key={id} className="check">
                  <input type="checkbox" checked={ticked.includes(id)} onChange={() => tick(id)} />
                  <span>{PREFLIGHT_TEXT[id] ?? id}</span>
                </label>
              ))}
            </div>
            <Toggle
              checked={settings.verifyAfterFlash}
              onChange={(verifyAfterFlash) => void updateSettings({ verifyAfterFlash })}
              label="Read back and compare after flashing"
              hint="Recommended. Adds a second transfer and shows whether the ECU holds your file."
            />
            <div className="field" style={{ maxWidth: 260 }}>
              <label htmlFor="confirm">Type {info.confirmPhrase} to confirm</label>
              <input
                id="confirm"
                className="input"
                value={confirm}
                autoComplete="off"
                spellCheck={false}
                placeholder={info.confirmPhrase}
                onChange={(e) => setConfirm(e.target.value)}
              />
            </div>
            <div className="row">
              <Button variant="danger" size="lg" icon="flash" disabled={notReady !== null} onClick={flash}>
                Flash ECU
              </Button>
              <span className="hint">
                {notReady ?? "Do not close the app, touch the cable or switch the ignition off until it finishes."}
              </span>
            </div>
            {startError && <Banner tone="danger">{startError}</Banner>}
          </section>
        </>
      )}
    </>
  );
}

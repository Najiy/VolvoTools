import { save } from "@tauri-apps/plugin-dialog";
import { useMemo, useState } from "react";
import { api, errorText } from "../api";
import { Banner } from "../components/Banner";
import { Button } from "../components/Button";
import { JobPanel } from "../components/JobPanel";
import { Outcome } from "../components/Outcome";
import { StopControl } from "../components/StopControl";
import { formatBytes, formatDuration, formatWhen, shortHash } from "../lib/format";
import { useApp } from "../state/AppState";
import type { BackupEntry } from "../types";

function whyUnverified(backup: BackupEntry): string {
  return backup.reads.length < 2 ? "Saved from a single read" : "The two reads did not match";
}

export function BackupView() {
  const { info, settings, backups, history, job, blockedReason, startBackup, cancel } = useApp();
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState<number | null>(null);

  const lastDuration = useMemo(
    () => history.find((entry) => entry.kind === "backup" && entry.ok)?.durationS ?? null,
    [history],
  );
  if (!info || !settings) return null;

  const backupJob = job.job?.kind === "backup";
  const showPanel = job.running && backupJob;
  const showOutcome = !job.running && backupJob && job.outcome !== null && dismissed !== job.job?.id;

  async function start() {
    setError(null);
    setStatus(null);
    try {
      await startBackup();
    } catch (e) {
      setError(errorText(e));
    }
  }

  async function exportBackup(backup: BackupEntry) {
    setError(null);
    try {
      const destination = await save({
        defaultPath: `${backup.id}.bin`,
        filters: [{ name: "ECU image", extensions: ["bin"] }],
      });
      if (!destination) return;
      await api.exportBackup(backup.id, destination);
      setStatus(`Exported ${backup.id} to ${destination}`);
    } catch (e) {
      setError(errorText(e));
    }
  }

  return (
    <>
      <header className="view-head">
        <h1>Backup</h1>
        <p className="lead">
          Read the whole engine ECU and keep the copy as verified only when two reads match byte for byte. A flash
          can only start from a verified backup.
        </p>
      </header>

      {error && <Banner tone="danger">{error}</Banner>}

      {showPanel && <JobPanel job={job} info={info} stop={<StopControl cancel={cancel} />} />}

      {showOutcome && job.outcome && (
        <Outcome outcome={job.outcome} kind="backup" onDismiss={() => setDismissed(job.job?.id ?? null)} />
      )}

      {!showPanel && (
        <section className="section" aria-labelledby="read-title">
          <div className="section-head">
            <h2 id="read-title">Read the ECU</h2>
          </div>
          <p className="section-note">
            Switch the ignition on and leave the engine off. Connect a battery charger. Do not use the car or
            touch the cable while it reads.
          </p>
          <div className="row">
            <Button variant="primary" size="lg" icon="read" disabled={blockedReason !== null} onClick={start}>
              {settings.verifyReads ? "Read ECU twice" : "Read ECU once"}
            </Button>
            <span className="hint">
              {blockedReason ??
                (lastDuration !== null
                  ? `Your last backup took ${formatDuration(lastDuration)}.`
                  : settings.verifyReads
                    ? "Two full reads. Plan for a while."
                    : "One read only. The copy will not count as verified.")}
            </span>
          </div>
        </section>
      )}

      <section className="section" aria-labelledby="library-title">
        <div className="section-head">
          <h2 id="library-title">Saved backups</h2>
        </div>
        {backups.length === 0 ? (
          <div className="empty">
            <strong>No backups yet</strong>
            <p>Read the ECU before changing anything. Keep a copy of the original off this PC as well.</p>
          </div>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Made</th>
                <th>Status</th>
                <th>SHA-256</th>
                <th className="num">Size</th>
                <th>
                  <span className="sr-only">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {backups.map((backup) => {
                const when = formatWhen(backup.createdAt);
                return (
                  <tr key={backup.id}>
                    <td>
                      {when.absolute}
                      <div className="hint">{when.relative}</div>
                    </td>
                    <td>
                      {backup.verified ? (
                        <span className="pill" data-tone="ok">
                          Verified
                        </span>
                      ) : (
                        <span className="pill" data-tone="warn" title={whyUnverified(backup)}>
                          Not verified
                        </span>
                      )}
                    </td>
                    <td className="mono" title={backup.sha256}>
                      {shortHash(backup.sha256)}
                    </td>
                    <td className="num">{formatBytes(backup.size)}</td>
                    <td>
                      <div className="actions">
                        <Button size="sm" variant="quiet" icon="folder" onClick={() => api.revealBackup(backup.id)}>
                          Show
                        </Button>
                        <Button size="sm" variant="quiet" icon="file" onClick={() => exportBackup(backup)}>
                          Export
                        </Button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {status && (
          <p className="hint" role="status">
            {status}
          </p>
        )}
      </section>
    </>
  );
}

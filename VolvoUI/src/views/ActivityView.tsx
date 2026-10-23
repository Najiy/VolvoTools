import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { Button } from "../components/Button";
import { formatDuration, formatWhen } from "../lib/format";
import { useApp } from "../state/AppState";

type Filter = "all" | "flasher" | "app";

export function ActivityView() {
  const { job, history } = useApp();
  const [filter, setFilter] = useState<Filter>("all");
  const [copied, setCopied] = useState(false);
  const logRef = useRef<HTMLDivElement>(null);
  const stuckToEnd = useRef(true);

  const lines = job.log.filter((l) => (filter === "all" ? true : filter === "app" ? l.stream === "app" : l.stream !== "app"));

  // Follow new lines unless the user has scrolled up to read.
  useEffect(() => {
    const el = logRef.current;
    if (el && stuckToEnd.current) el.scrollTop = el.scrollHeight;
  }, [lines.length]);

  function onScroll() {
    const el = logRef.current;
    if (el) stuckToEnd.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(lines.map((l) => l.line).join("\n"));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  }

  return (
    <>
      <header className="view-head">
        <h1>Activity</h1>
        <p className="lead">What the app and the flasher program did during the current or last job.</p>
      </header>

      <section className="section" aria-labelledby="log-title">
        <div className="section-head">
          <h2 id="log-title">Log</h2>
        </div>
        <div className="row spread">
          <div className="segmented" role="group" aria-label="Log filter" style={{ width: 260 }}>
            {(["all", "flasher", "app"] as const).map((value) => (
              <button key={value} type="button" aria-pressed={filter === value} onClick={() => setFilter(value)}>
                {value === "all" ? "All" : value === "flasher" ? "Flasher" : "App"}
              </button>
            ))}
          </div>
          <div className="row">
            <Button size="sm" icon="copy" onClick={copy} disabled={lines.length === 0}>
              {copied ? "Copied" : "Copy"}
            </Button>
            <Button size="sm" icon="folder" onClick={() => api.revealLogs()}>
              Open log folder
            </Button>
          </div>
        </div>
        {lines.length === 0 ? (
          <div className="empty">
            <strong>Nothing logged yet</strong>
            <p>Commands, flasher output and results appear here while a job runs.</p>
          </div>
        ) : (
          <div className="log" ref={logRef} onScroll={onScroll} role="log" aria-label="Job log">
            {lines.map((l, index) => (
              <div key={index} data-stream={l.stream}>
                {l.line}
              </div>
            ))}
          </div>
        )}
      </section>

      <section className="section" aria-labelledby="history-title">
        <div className="section-head">
          <h2 id="history-title">Finished jobs</h2>
        </div>
        {history.length === 0 ? (
          <div className="empty">
            <strong>No jobs yet</strong>
            <p>Every backup and flash is recorded here with the files and checksums involved.</p>
          </div>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th className="nowrap">When</th>
                <th>Job</th>
                <th>Result</th>
                <th className="num nowrap">Took</th>
              </tr>
            </thead>
            <tbody>
              {history.map((entry) => (
                <tr key={`${entry.time}-${entry.kind}`}>
                  <td className="nowrap">{formatWhen(entry.time).absolute}</td>
                  <td>{entry.kind === "flash" ? "Flash" : "Backup"}</td>
                  <td>
                    <span className="pill" data-tone={entry.ok ? "ok" : "danger"}>
                      {entry.ok ? "Done" : "Failed"}
                    </span>{" "}
                    {entry.message}
                    {entry.detail && <div className="hint mono">{entry.detail}</div>}
                  </td>
                  <td className="num nowrap">{formatDuration(entry.durationS)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </>
  );
}

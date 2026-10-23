import { hex, plural, shortHash } from "../lib/format";
import type { JobKind, JobOutcome } from "../types";
import { Banner } from "./Banner";
import { Button } from "./Button";

interface OutcomeProps {
  outcome: JobOutcome;
  kind: JobKind;
  onDismiss: () => void;
}

function titleFor(outcome: JobOutcome, kind: JobKind): string {
  if (outcome.ok) return kind === "flash" ? "Flash complete" : "Backup complete";
  if (outcome.cancelled) return "Stopped";
  return kind === "flash" ? "Flash did not complete" : "Backup did not complete";
}

export function Outcome({ outcome, kind, onDismiss }: OutcomeProps) {
  const tone = outcome.ok ? "ok" : outcome.cancelled ? "warn" : "danger";
  const verify = outcome.verify;
  return (
    <div className="stack">
      <Banner
        tone={tone}
        title={titleFor(outcome, kind)}
        actions={
          <Button size="sm" variant="quiet" onClick={onDismiss}>
            Dismiss
          </Button>
        }
      >
        <p>{outcome.message}</p>
        {outcome.hint && <p className="hint">{outcome.hint}</p>}
        {outcome.backup && (
          <p className="hint">
            Saved as <span className="mono">{outcome.backup.id}</span>, SHA-256{" "}
            <span className="mono">{shortHash(outcome.backup.sha256, 16)}</span>
          </p>
        )}
      </Banner>
      {verify && (
        <table className="table" aria-label="Read-back comparison">
          <thead>
            <tr>
              <th>Area</th>
              <th>Range</th>
              <th>Compared with</th>
              <th>Result</th>
            </tr>
          </thead>
          <tbody>
            {verify.checks.map((check) => (
              <tr key={check.name}>
                <td>{check.name}</td>
                <td className="mono">
                  {hex(check.start)} to {hex(check.end - 1)}
                </td>
                <td>{check.expected === "tune" ? "Your file" : "Your backup"}</td>
                <td>
                  {check.mismatched === 0 ? (
                    <span className="pill" data-tone="ok">
                      Matches
                    </span>
                  ) : (
                    <span className="pill" data-tone={check.expected === "tune" ? "danger" : "warn"}>
                      {plural(check.mismatched, "byte")} {check.mismatched === 1 ? "differs" : "differ"}
                      {check.firstDiff !== null ? `, first at ${hex(check.firstDiff)}` : ""}
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

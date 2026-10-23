import { useEffect, useState, type CSSProperties, type ReactNode } from "react";
import { etaSeconds } from "../lib/eta";
import { formatClock, formatCount, formatDuration, percent } from "../lib/format";
import type { JobState } from "../lib/jobState";
import { stageIndex, stagesFor } from "../lib/stages";
import type { AppInfo } from "../types";
import { Icon } from "./Icon";
import { SectorStrip } from "./SectorStrip";

/** Seconds since `startedAtMs`, ticking once a second while `running`. */
function useElapsed(startedAtMs: number | null, running: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [running]);
  return startedAtMs === null ? 0 : Math.max(0, (now - startedAtMs) / 1000);
}

interface JobPanelProps {
  job: JobState;
  info: AppInfo;
  /** The stop control, rendered under the progress. */
  stop: ReactNode;
}

export function JobPanel({ job, info, stop }: JobPanelProps) {
  const mode = job.phase?.mode ?? "read";
  const stages = stagesFor(mode);
  const active = stageIndex(mode, job.state);
  const progress = job.progress;
  const fraction = progress && progress.max > 0 ? Math.min(1, progress.current / progress.max) : 0;
  const pct = progress ? percent(progress.current, progress.max) : 0;
  const elapsed = useElapsed(job.job?.startedAtMs ?? null, job.running);
  const eta = progress ? etaSeconds(job.samples, progress.max) : null;
  const stageLabel = active >= 0 ? stages[active].label : "Starting";
  const phaseLabel = job.phase?.label ?? "Starting";
  const range: [number, number] | undefined =
    mode === "read"
      ? job.job?.kind === "flash"
        ? info.profile.verifyRange
        : [0, info.profile.imageSize]
      : undefined;

  return (
    <section className="job" aria-label="Job progress">
      <div className="job-main">
        <div className="job-phase">
          {phaseLabel}
          {job.phase && job.phase.total > 1 && !/\d+ of \d+/.test(phaseLabel)
            ? ` (step ${job.phase.index} of ${job.phase.total})`
            : ""}
        </div>
        <div className="job-readout">
          <span className="job-pct">{pct}%</span>
          <span className="job-stage">{stageLabel}</span>
        </div>
        <div
          className="bar"
          role="progressbar"
          aria-label={`${phaseLabel} progress`}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct}
        >
          <div className="bar-fill" style={{ "--p": fraction } as CSSProperties} />
        </div>
        <div className="job-meta">
          <span>Elapsed {formatClock(elapsed)}</span>
          <span>{eta === null ? "Estimating time left" : `About ${formatDuration(eta)} left`}</span>
          {progress && (
            <span>
              {formatCount(progress.current)} of {formatCount(progress.max)} bytes
            </span>
          )}
        </div>
        {range && (
          <SectorStrip
            imageSize={info.profile.imageSize}
            sectorSize={info.sectorSize}
            regions={info.profile.regions}
            label={`Memory read progress, ${pct} percent`}
            progress={fraction}
            range={range}
            showRoles={false}
          />
        )}
        <div className="sr-only" aria-live="polite">
          {phaseLabel}: {stageLabel}
        </div>
        {stop}
      </div>
      <ol className="stages" aria-label="Stages">
        {stages.map((stage, index) => {
          const state = index < active ? "done" : index === active ? "active" : "pending";
          return (
            <li key={stage.id} className="stage" data-state={state} aria-current={state === "active" ? "step" : undefined}>
              <span className="stage-mark">{state === "done" && <Icon name="check" size={12} />}</span>
              {stage.label}
            </li>
          );
        })}
      </ol>
    </section>
  );
}

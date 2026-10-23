import type { JobEvent, JobInfo, JobOutcome, JobSnapshot, LogLine, PhaseInfo, ProgressInfo } from "../types";
import { pushSample, type Sample } from "./eta";

const LOG_LIMIT = 1000;

export interface JobState {
  running: boolean;
  job: JobInfo | null;
  phase: PhaseInfo | null;
  state: string | null;
  progress: ProgressInfo | null;
  log: LogLine[];
  outcome: JobOutcome | null;
  /** Progress samples of the current phase, for the remaining-time estimate. */
  samples: Sample[];
}

export const initialJob: JobState = {
  running: false,
  job: null,
  phase: null,
  state: null,
  progress: null,
  log: [],
  outcome: null,
  samples: [],
};

export type JobAction =
  | { type: "event"; event: JobEvent; at: number }
  | { type: "snapshot"; snapshot: JobSnapshot; at: number };

function appendLog(log: LogLine[], line: LogLine): LogLine[] {
  const next = log.length >= LOG_LIMIT ? log.slice(log.length - LOG_LIMIT + 1) : log.slice();
  next.push(line);
  return next;
}

export function jobReducer(state: JobState, action: JobAction): JobState {
  if (action.type === "snapshot") {
    const { snapshot, at } = action;
    return {
      running: snapshot.running,
      job: snapshot.job,
      phase: snapshot.phase,
      state: snapshot.state,
      progress: snapshot.progress,
      log: snapshot.log.slice(-LOG_LIMIT),
      outcome: snapshot.outcome,
      samples: snapshot.progress ? [{ t: at, value: snapshot.progress.current }] : [],
    };
  }

  const { event, at } = action;
  switch (event.kind) {
    case "started":
      return { ...initialJob, running: true, job: event.job };
    case "phase":
      return {
        ...state,
        phase: { label: event.label, index: event.index, total: event.total, mode: event.mode },
        state: null,
        progress: null,
        samples: [],
      };
    case "state":
      return { ...state, state: event.state };
    case "progress":
      return {
        ...state,
        progress: { current: event.current, max: event.max },
        samples: pushSample(state.samples, at, event.current),
      };
    case "log":
      return { ...state, log: appendLog(state.log, { stream: event.stream, line: event.line }) };
    case "finished":
      return { ...state, running: false, outcome: event.outcome };
  }
}

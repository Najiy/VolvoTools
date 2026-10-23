export interface Sample {
  /** Milliseconds, any monotonic origin. */
  t: number;
  value: number;
}

const WINDOW_MS = 60_000;
const MIN_SPAN_MS = 4_000;

/** Appends a sample and drops those older than the averaging window. */
export function pushSample(samples: Sample[], t: number, value: number): Sample[] {
  const kept = samples.filter((sample) => t - sample.t <= WINDOW_MS);
  kept.push({ t, value });
  return kept;
}

/** Seconds until `max` is reached at the recent rate, or null while there is too little data. */
export function etaSeconds(samples: Sample[], max: number): number | null {
  if (samples.length < 2) return null;
  const first = samples[0];
  const last = samples[samples.length - 1];
  const span = last.t - first.t;
  const gained = last.value - first.value;
  if (span < MIN_SPAN_MS || gained <= 0) return null;
  const rate = gained / (span / 1000);
  return Math.max(0, (max - last.value) / rate);
}

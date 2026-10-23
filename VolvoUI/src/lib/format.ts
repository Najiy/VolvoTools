export function hex(value: number, width = 6): string {
  return "0x" + value.toString(16).toUpperCase().padStart(width, "0");
}

export function formatCount(value: number): string {
  return value.toLocaleString("en-US");
}

export function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(value < 10 * 1024 ? 1 : 0)} KB`;
  return `${(value / (1024 * 1024)).toFixed(2)} MB`;
}

/** "45 s", "12 min", "1 h 05 min". */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s} s`;
  const minutes = Math.round(s / 60);
  if (minutes < 60) return `${minutes} min`;
  const h = Math.floor(minutes / 60);
  return `${h} h ${String(minutes % 60).padStart(2, "0")} min`;
}

/** "02:07" or "1:02:07", for a running clock. */
export function formatClock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rest = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${rest}` : `${String(m).padStart(2, "0")}:${rest}`;
}

export function percent(current: number, max: number): number {
  if (max <= 0) return 0;
  return Math.max(0, Math.min(100, Math.floor((current / max) * 100)));
}

export function shortHash(hash: string, length = 12): string {
  return hash.slice(0, length);
}

export function formatWhen(iso: string, now = new Date()): { absolute: string; relative: string } {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return { absolute: iso, relative: "" };
  const absolute = date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
  const minutes = Math.round((now.getTime() - date.getTime()) / 60000);
  let relative: string;
  if (minutes < 1) relative = "just now";
  else if (minutes < 60) relative = `${minutes} min ago`;
  else if (minutes < 60 * 24) relative = `${Math.round(minutes / 60)} h ago`;
  else relative = `${Math.round(minutes / (60 * 24))} d ago`;
  return { absolute, relative };
}

export function plural(count: number, one: string, many = `${one}s`): string {
  return `${formatCount(count)} ${count === 1 ? one : many}`;
}

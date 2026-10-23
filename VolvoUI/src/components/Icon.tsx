export type IconName =
  | "check"
  | "cross"
  | "warn"
  | "info"
  | "read"
  | "flash"
  | "plug"
  | "folder"
  | "copy"
  | "stop"
  | "file"
  | "refresh"
  | "log";

const SHAPES: Record<IconName, string[]> = {
  check: ["M3 8.5l3.2 3.2L13 4.8"],
  cross: ["M4 4l8 8", "M12 4l-8 8"],
  warn: ["M8 2.2l6.3 11H1.7L8 2.2z", "M8 6.6v3.2", "M8 11.7h.01"],
  info: ["M2 8a6 6 0 1 0 12 0a6 6 0 1 0-12 0", "M8 7.3V11", "M8 4.9h.01"],
  read: ["M8 2.5v8", "M4.8 7.6L8 10.8l3.2-3.2", "M3 13.5h10"],
  flash: ["M9 1.8L3.5 9H8l-1 5.2L12.5 7H8l1-5.2z"],
  plug: ["M6 2v3", "M10 2v3", "M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0V5z", "M8 11v3"],
  folder: ["M1.8 4.2h4l1.4 1.6h7v6.8H1.8V4.2z"],
  copy: ["M5.5 5.5h7v8h-7z", "M3.5 10.5v-8h7"],
  stop: ["M4 4h8v8H4z"],
  file: ["M4 1.8h5l3 3v9.4H4V1.8z", "M9 1.8v3h3"],
  refresh: ["M13 8a5 5 0 1 1-1.6-3.7", "M13 2.8v2.6h-2.6"],
  log: ["M3 4h10", "M3 8h10", "M3 12h6"],
};

export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width={size}
      height={size}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {SHAPES[name].map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  );
}

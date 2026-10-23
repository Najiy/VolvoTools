// Mirrors the serde output of the volvo-core crate (camelCase). Keep in sync with Rust.

export type JobKind = "backup" | "flash";
export type PhaseMode = "read" | "flash";
export type LogStream = "stdout" | "stderr" | "app";
export type RegionRole = "written" | "notWritten";

export interface Region {
  name: string;
  start: number;
  end: number;
  role: RegionRole;
}

export interface Profile {
  id: string;
  title: string;
  platform: string;
  ecu: string;
  cmInfo: string;
  baudrate: number;
  imageSize: number;
  regions: Region[];
  verifyRange: [number, number];
}

export interface AppInfo {
  version: string;
  profile: Profile;
  defaultBackupsDir: string;
  logDir: string;
  confirmPhrase: string;
  preflightItems: string[];
  sectorSize: number;
}

export interface Settings {
  flasherPath: string | null;
  backupsDir: string | null;
  device: string | null;
  verifyReads: boolean;
  verifyAfterFlash: boolean;
  verbose: boolean;
}

export interface Device {
  name: string;
  library: string;
}

export interface FlasherStatus {
  path: string | null;
  source: "env" | "settings" | "auto" | null;
  found: boolean;
  compatible: boolean;
  version: string | null;
  error: string | null;
}

export interface ScanResult {
  flasher: FlasherStatus;
  devices: Device[];
}

export interface BackupEntry {
  id: string;
  createdAt: string;
  profile: string;
  size: number;
  sha256: string;
  verified: boolean;
  reads: string[];
  device: string;
  note: string;
  path: string;
  dir: string;
}

export interface RegionDiff {
  name: string;
  start: number;
  end: number;
  role: RegionRole;
  changed: number;
  firstDiff: number | null;
}

export interface DiffReport {
  regions: RegionDiff[];
  sectors: number[];
  totalChanged: number;
  changedInWritten: number;
  changedInNotWritten: number;
  writtenBytes: number;
  identical: boolean;
}

export type ChecksumStatus = "valid" | "correctable" | "unsupported" | "unavailable";

export interface ChecksumVerdict {
  status: ChecksumStatus;
  modifiedBytes: number;
  /** Records the flasher really evaluated. Zero means the check looked at nothing. */
  checkedBlocks: number;
  detail: string | null;
}

export interface FileInfo {
  name: string;
  path: string;
  size: number;
  sha256: string;
}

export interface FlashPreview {
  file: FileInfo;
  backupId: string;
  diff: DiffReport | null;
  tuneChecksum: ChecksumVerdict | null;
  backupChecksum: ChecksumVerdict | null;
  blockers: string[];
  warnings: string[];
  requiresChecksumAck: boolean;
  canFlash: boolean;
}

export interface VerifyCheck {
  name: string;
  start: number;
  end: number;
  expected: "tune" | "backup";
  mismatched: number;
  firstDiff: number | null;
}

export interface VerifyReport {
  checks: VerifyCheck[];
  writtenOk: boolean;
  untouchedChanged: boolean;
}

export interface JobOutcome {
  ok: boolean;
  cancelled: boolean;
  message: string;
  hint: string | null;
  backup: BackupEntry | null;
  verify: VerifyReport | null;
}

export interface JobInfo {
  id: number;
  kind: JobKind;
  startedAtMs: number;
}

export interface PhaseInfo {
  label: string;
  index: number;
  total: number;
  mode: PhaseMode;
}

export interface ProgressInfo {
  current: number;
  max: number;
}

export interface LogLine {
  stream: LogStream;
  line: string;
}

export type JobEvent =
  | { kind: "started"; job: JobInfo }
  | ({ kind: "phase" } & PhaseInfo)
  | { kind: "state"; state: string }
  | ({ kind: "progress" } & ProgressInfo)
  | ({ kind: "log" } & LogLine)
  | { kind: "finished"; outcome: JobOutcome };

export interface JobSnapshot {
  running: boolean;
  job: JobInfo | null;
  phase: PhaseInfo | null;
  state: string | null;
  progress: ProgressInfo | null;
  log: LogLine[];
  outcome: JobOutcome | null;
}

export interface HistoryEntry {
  time: string;
  kind: JobKind;
  ok: boolean;
  message: string;
  detail: string;
  durationS: number;
}

export interface FlashRequest {
  device: string;
  tunePath: string;
  backupId: string;
  confirm: string;
  preflight: string[];
  verifyAfter: boolean;
  allowChecksumChanges: boolean;
}

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppInfo,
  BackupEntry,
  FlashPreview,
  FlashRequest,
  HistoryEntry,
  JobEvent,
  JobInfo,
  JobSnapshot,
  ScanResult,
  Settings,
} from "./types";

/** Errors from the backend arrive as plain strings. */
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<Settings>("save_settings", { settings }),
  scan: () => invoke<ScanResult>("scan"),
  listBackups: () => invoke<BackupEntry[]>("list_backups"),
  listHistory: () => invoke<HistoryEntry[]>("list_history"),
  currentJob: () => invoke<JobSnapshot>("current_job"),
  previewFlash: (tunePath: string, backupId: string) =>
    invoke<FlashPreview>("preview_flash", { tunePath, backupId }),
  startBackup: (device: string, verify: boolean) => invoke<JobInfo>("start_backup", { device, verify }),
  startFlash: (request: FlashRequest) => invoke<JobInfo>("start_flash", { request }),
  cancelJob: (force: boolean) => invoke<void>("cancel_job", { force }),
  exportBackup: (id: string, destination: string) => invoke<void>("export_backup", { id, destination }),
  revealBackup: (id: string) => invoke<void>("reveal_backup", { id }),
  revealBackups: () => invoke<void>("reveal_backups"),
  revealLogs: () => invoke<void>("reveal_logs"),
};

export function onJobEvent(handler: (event: JobEvent) => void): Promise<UnlistenFn> {
  return listen<JobEvent>("job-event", (message) => handler(message.payload));
}

export function onCloseBlocked(handler: () => void): Promise<UnlistenFn> {
  return listen("close-blocked", () => handler());
}

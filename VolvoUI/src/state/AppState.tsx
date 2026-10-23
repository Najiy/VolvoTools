import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api, errorText, onCloseBlocked, onJobEvent } from "../api";
import { initialJob, jobReducer, type JobState } from "../lib/jobState";
import type {
  AppInfo,
  BackupEntry,
  Device,
  FlashRequest,
  HistoryEntry,
  ScanResult,
  Settings,
} from "../types";

/** What the Flash view has chosen so far. Kept here so it survives switching views. */
export interface FlashDraft {
  tunePath: string;
  backupId: string | null;
}

interface AppValue {
  ready: boolean;
  loadError: string | null;
  info: AppInfo | null;
  settings: Settings | null;
  scan: ScanResult | null;
  scanning: boolean;
  backups: BackupEntry[];
  verifiedBackups: BackupEntry[];
  history: HistoryEntry[];
  job: JobState;
  closeBlocked: boolean;
  /** The adapter that jobs will use: the saved choice if it is still present. */
  device: Device | null;
  /** Why a job cannot start right now, or null when it can. */
  blockedReason: string | null;
  draft: FlashDraft;
  setDraft: (patch: Partial<FlashDraft>) => void;
  rescan: () => Promise<void>;
  updateSettings: (patch: Partial<Settings>) => Promise<void>;
  refreshBackups: () => Promise<void>;
  startBackup: () => Promise<void>;
  startFlash: (request: Omit<FlashRequest, "device">) => Promise<void>;
  cancel: (force: boolean) => Promise<void>;
}

const AppContext = createContext<AppValue | null>(null);

export function useApp(): AppValue {
  const value = useContext(AppContext);
  if (!value) throw new Error("useApp must be used inside AppProvider");
  return value;
}

export function AppProvider({ children }: { children: ReactNode }) {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [scan, setScan] = useState<ScanResult | null>(null);
  const [scanning, setScanning] = useState(false);
  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [job, dispatch] = useReducer(jobReducer, initialJob);
  const [closeBlocked, setCloseBlocked] = useState(false);
  const [ready, setReady] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [draft, setDraftState] = useState<FlashDraft>({ tunePath: "", backupId: null });
  const settingsRef = useRef<Settings | null>(null);
  settingsRef.current = settings;

  const refreshBackups = useCallback(async () => {
    const [list, past] = await Promise.all([api.listBackups(), api.listHistory()]);
    setBackups(list);
    setHistory(past);
  }, []);

  const rescan = useCallback(async () => {
    setScanning(true);
    try {
      setScan(await api.scan());
    } catch (error) {
      setScan({
        flasher: { path: null, source: null, found: false, compatible: false, version: null, error: errorText(error) },
        devices: [],
      });
    } finally {
      setScanning(false);
    }
  }, []);

  const updateSettings = useCallback(async (patch: Partial<Settings>) => {
    const current = settingsRef.current;
    if (!current) return;
    setSettings(await api.saveSettings({ ...current, ...patch }));
  }, []);

  // Initial load, then keep in step with the backend's job events.
  useEffect(() => {
    let unlisten: Array<() => void> = [];
    let disposed = false;
    (async () => {
      try {
        const [appInfo, loaded] = await Promise.all([api.appInfo(), api.getSettings()]);
        if (disposed) return;
        setInfo(appInfo);
        setSettings(loaded);
        const handlers = await Promise.all([
          onJobEvent((event) => {
            dispatch({ type: "event", event, at: Date.now() });
            if (event.kind === "finished") void refreshBackups();
          }),
          onCloseBlocked(() => setCloseBlocked(true)),
        ]);
        if (disposed) {
          handlers.forEach((fn) => fn());
          return;
        }
        unlisten = handlers;
        dispatch({ type: "snapshot", snapshot: await api.currentJob(), at: Date.now() });
        await refreshBackups();
        setReady(true);
        void rescan();
      } catch (error) {
        setLoadError(errorText(error));
        setReady(true);
      }
    })();
    return () => {
      disposed = true;
      unlisten.forEach((fn) => fn());
    };
  }, [refreshBackups, rescan]);

  // The close warning goes away by itself once the job is over.
  useEffect(() => {
    if (!job.running) setCloseBlocked(false);
  }, [job.running]);

  const device = useMemo<Device | null>(() => {
    const devices = scan?.devices ?? [];
    return devices.find((d) => d.name === settings?.device) ?? (devices.length === 1 ? devices[0] : null);
  }, [scan, settings?.device]);

  const verifiedBackups = useMemo(() => backups.filter((b) => b.verified), [backups]);

  const blockedReason = useMemo(() => {
    if (job.running) return "A job is running.";
    if (!scan) return "Looking for the flasher program.";
    if (!scan.flasher.found) return "VolvoFlasher.exe was not found. Choose it in Connection.";
    if (!scan.flasher.compatible) return "This VolvoFlasher build cannot be driven by the app. See Connection.";
    if (!device) return scan.devices.length === 0 ? "No J2534 adapter was found." : "Choose an adapter in Connection.";
    return null;
  }, [job.running, scan, device]);

  const startBackup = useCallback(async () => {
    if (!device || !settingsRef.current) return;
    try {
      await api.startBackup(device.name, settingsRef.current.verifyReads);
    } catch (error) {
      throw new Error(errorText(error));
    }
  }, [device]);

  const startFlash = useCallback(
    async (request: Omit<FlashRequest, "device">) => {
      if (!device) return;
      try {
        await api.startFlash({ ...request, device: device.name });
      } catch (error) {
        throw new Error(errorText(error));
      }
    },
    [device],
  );

  const cancel = useCallback(async (force: boolean) => {
    try {
      await api.cancelJob(force);
    } catch (error) {
      throw new Error(errorText(error));
    }
  }, []);

  const setDraft = useCallback((patch: Partial<FlashDraft>) => setDraftState((d) => ({ ...d, ...patch })), []);

  // Remember the adapter when it was picked implicitly because it is the only one.
  useEffect(() => {
    if (device && settings && settings.device !== device.name) void updateSettings({ device: device.name });
  }, [device, settings, updateSettings]);

  const value: AppValue = {
    ready,
    loadError,
    info,
    settings,
    scan,
    scanning,
    backups,
    verifiedBackups,
    history,
    job,
    closeBlocked,
    device,
    blockedReason,
    draft,
    setDraft,
    rescan,
    updateSettings,
    refreshBackups,
    startBackup,
    startFlash,
    cancel,
  };
  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

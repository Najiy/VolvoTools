import { useEffect, useRef, useState } from "react";
import { Banner } from "./components/Banner";
import { Icon, type IconName } from "./components/Icon";
import { percent } from "./lib/format";
import { useTheme, type Theme } from "./lib/theme";
import { AppProvider, useApp } from "./state/AppState";
import { ActivityView } from "./views/ActivityView";
import { BackupView } from "./views/BackupView";
import { ConnectionView } from "./views/ConnectionView";
import { FlashView } from "./views/FlashView";

export type View = "connection" | "backup" | "flash" | "activity";

interface NavItem {
  id: View;
  label: string;
  number?: number;
  icon?: IconName;
  done?: boolean;
  needsAttention?: boolean;
  busy?: boolean;
}

const THEMES: Array<{ id: Theme; label: string }> = [
  { id: "system", label: "Auto" },
  { id: "dark", label: "Dark" },
  { id: "light", label: "Light" },
];

function Shell() {
  const app = useApp();
  const [view, setView] = useState<View>("connection");
  const [theme, setTheme] = useTheme();
  const placed = useRef(false);
  const { ready, scan, blockedReason, job } = app;

  // Open where the work is: the running job, otherwise Backup once the setup is complete.
  useEffect(() => {
    if (placed.current || !ready || !scan) return;
    placed.current = true;
    if (job.running && job.job) setView(job.job.kind);
    else if (blockedReason === null) setView("backup");
  }, [ready, scan, blockedReason, job.running, job.job]);

  if (!ready) return null;
  if (app.loadError || !app.info || !app.settings) {
    return (
      <main className="main">
        <div className="main-inner">
          <Banner tone="danger" title="The app could not start">
            <p>{app.loadError ?? "The backend did not respond."}</p>
          </Banner>
        </div>
      </main>
    );
  }

  const connected = Boolean(scan?.flasher.compatible && app.device);
  const items: NavItem[] = [
    { id: "connection", label: "Connection", number: 1, done: connected, needsAttention: !connected },
    {
      id: "backup",
      label: "Backup",
      number: 2,
      done: app.verifiedBackups.length > 0,
      busy: job.running && job.job?.kind === "backup",
    },
    { id: "flash", label: "Flash", number: 3, busy: job.running && job.job?.kind === "flash" },
    { id: "activity", label: "Activity", icon: "log" },
  ];

  const pct = job.progress ? percent(job.progress.current, job.progress.max) : 0;
  const jobSummary = job.running
    ? `${job.phase?.label ?? "Starting"}, ${pct}%`
    : job.outcome
      ? `Last job: ${job.outcome.ok ? "done" : job.outcome.cancelled ? "stopped" : "failed"}`
      : "No job running";

  const flasher = scan?.flasher;
  const flasherTone = !scan ? "idle" : flasher?.compatible ? "ok" : "warn";

  return (
    <div className="app">
      <nav className="nav" aria-label="Main">
        <div className="brand">
          <strong>VolvoTools</strong>
          <span>{app.info.profile.title}</span>
        </div>
        <ul className="nav-list">
          {items.map((item) => (
            <li key={item.id}>
              <button
                type="button"
                className="nav-item"
                aria-current={view === item.id ? "page" : undefined}
                data-done={item.done ? "true" : undefined}
                onClick={() => setView(item.id)}
              >
                <span className="nav-no">
                  {item.done ? <Icon name="check" size={12} /> : item.icon ? <Icon name={item.icon} size={12} /> : item.number}
                </span>
                <span>{item.label}</span>
                {item.busy ? (
                  <span className="nav-flag" data-tone="busy" title="Running" />
                ) : item.needsAttention ? (
                  <span className="nav-flag" title="Needs attention" />
                ) : null}
              </button>
            </li>
          ))}
        </ul>
        <div className="nav-foot">
          <div className="segmented" role="group" aria-label="Theme">
            {THEMES.map((t) => (
              <button key={t.id} type="button" aria-pressed={theme === t.id} onClick={() => setTheme(t.id)}>
                {t.label}
              </button>
            ))}
          </div>
          <small>Version {app.info.version}</small>
        </div>
      </nav>

      <main className="main">
        <div className="main-inner">
          {flasher?.version === "fake" && (
            <Banner tone="info" title="Simulator">
              <p>No real ECU is involved. Backups and flashes only change a fake memory file on this PC.</p>
            </Banner>
          )}
          {app.closeBlocked && (
            <Banner tone="warn" title="A job is running">
              <p>The window stays open until it finishes. Stop the job first if you need to close the app.</p>
            </Banner>
          )}
          {view === "connection" && <ConnectionView />}
          {view === "backup" && <BackupView />}
          {view === "flash" && <FlashView go={setView} />}
          {view === "activity" && <ActivityView />}
        </div>
      </main>

      <footer className="statusbar">
        <span>
          <i className="dot" data-tone={app.device ? "ok" : "warn"} />
          {app.device ? app.device.name : "No adapter selected"}
          <span className="faint">|</span>
          <i className="dot" data-tone={flasherTone === "idle" ? undefined : flasherTone} />
          {!scan ? "Looking for the flasher" : flasher?.compatible ? "Flasher ready" : "Flasher not ready"}
        </span>
        <span>
          <i className="dot" data-tone={job.running ? "busy" : undefined} />
          {jobSummary}
        </span>
      </footer>
    </div>
  );
}

export default function App() {
  return (
    <AppProvider>
      <Shell />
    </AppProvider>
  );
}

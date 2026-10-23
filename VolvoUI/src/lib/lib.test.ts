import { describe, expect, it } from "vitest";
import { etaSeconds, pushSample } from "./eta";
import { formatBytes, formatClock, formatDuration, formatWhen, hex, percent, plural } from "./format";
import { initialJob, jobReducer, type JobState } from "./jobState";
import { stageIndex, stagesFor } from "./stages";
import type { JobEvent, JobOutcome } from "../types";

const at = (ms: number) => ms;
const apply = (state: JobState, event: JobEvent, t = 0) => jobReducer(state, { type: "event", event, at: at(t) });

const outcome: JobOutcome = { ok: true, cancelled: false, message: "done", hint: null, backup: null, verify: null };

describe("format", () => {
  it("formats addresses with fixed width", () => {
    expect(hex(0x20000)).toBe("0x020000");
    expect(hex(255, 2)).toBe("0xFF");
  });

  it("formats sizes", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(2 * 1024 * 1024)).toBe("2.00 MB");
  });

  it("formats durations and clocks", () => {
    expect(formatDuration(40)).toBe("40 s");
    expect(formatDuration(12 * 60 + 20)).toBe("12 min");
    expect(formatDuration(3900)).toBe("1 h 05 min");
    expect(formatClock(127)).toBe("02:07");
    expect(formatClock(3727)).toBe("1:02:07");
  });

  it("clamps percentages", () => {
    expect(percent(50, 200)).toBe(25);
    expect(percent(5, 0)).toBe(0);
    expect(percent(300, 200)).toBe(100);
  });

  it("pluralizes", () => {
    expect(plural(1, "byte")).toBe("1 byte");
    expect(plural(1234, "byte")).toBe("1,234 bytes");
  });

  it("describes when something happened", () => {
    const now = new Date("2026-10-23T12:00:00Z");
    expect(formatWhen("2026-10-23T11:30:00Z", now).relative).toBe("30 min ago");
    expect(formatWhen("2026-10-23T08:00:00Z", now).relative).toBe("4 h ago");
    expect(formatWhen("nonsense", now)).toEqual({ absolute: "nonsense", relative: "" });
  });
});

describe("eta", () => {
  it("needs enough data before it guesses", () => {
    expect(etaSeconds([], 100)).toBeNull();
    expect(etaSeconds([{ t: 0, value: 0 }, { t: 1000, value: 10 }], 100)).toBeNull();
  });

  it("extrapolates the recent rate", () => {
    let samples = pushSample([], 0, 0);
    samples = pushSample(samples, 10_000, 100);
    expect(etaSeconds(samples, 1000)).toBeCloseTo(90, 5);
  });

  it("returns null when nothing is moving", () => {
    let samples = pushSample([], 0, 50);
    samples = pushSample(samples, 10_000, 50);
    expect(etaSeconds(samples, 100)).toBeNull();
  });

  it("forgets samples outside the window", () => {
    let samples = pushSample([], 0, 0);
    samples = pushSample(samples, 100_000, 500);
    expect(samples).toHaveLength(1);
  });
});

describe("stages", () => {
  it("treats erase and write as reading during a read", () => {
    const read = stagesFor("read");
    expect(read[stageIndex("read", "EraseFlash")].id).toBe("read");
    expect(read[stageIndex("read", "WriteFlash")].id).toBe("read");
  });

  it("keeps erase and write apart during a flash", () => {
    expect(stagesFor("flash")[stageIndex("flash", "EraseFlash")].id).toBe("erase");
    expect(stagesFor("flash")[stageIndex("flash", "WriteFlash")].id).toBe("write");
  });

  it("has no stage before the first state and for unknown states", () => {
    expect(stageIndex("read", null)).toBe(-1);
    expect(stageIndex("read", "Surprise")).toBe(-1);
  });

  it("orders stages as the flasher runs them", () => {
    const order = ["FallAsleep", "OpenChannels", "LoadBootloader", "StartBootloader", "WriteFlash", "WakeUp"];
    const indexes = order.map((state) => stageIndex("read", state));
    expect(indexes).toEqual([...indexes].sort((a, b) => a - b));
    expect(new Set(indexes).size).toBe(order.length);
  });
});

describe("job reducer", () => {
  const started: JobEvent = { kind: "started", job: { id: 1, kind: "backup", startedAtMs: 0 } };

  it("follows a job from start to finish", () => {
    let s = apply(initialJob, started);
    expect(s.running).toBe(true);
    s = apply(s, { kind: "phase", label: "Read 1 of 2", index: 1, total: 2, mode: "read" });
    s = apply(s, { kind: "state", state: "LoadBootloader" });
    s = apply(s, { kind: "progress", current: 10, max: 100 }, 1000);
    expect(s.phase?.label).toBe("Read 1 of 2");
    expect(s.state).toBe("LoadBootloader");
    expect(s.progress).toEqual({ current: 10, max: 100 });
    expect(s.samples).toHaveLength(1);
    s = apply(s, { kind: "finished", outcome });
    expect(s.running).toBe(false);
    expect(s.outcome?.message).toBe("done");
  });

  it("resets progress when the next phase starts", () => {
    let s = apply(initialJob, started);
    s = apply(s, { kind: "phase", label: "Read 1 of 2", index: 1, total: 2, mode: "read" });
    s = apply(s, { kind: "progress", current: 90, max: 100 }, 500);
    s = apply(s, { kind: "phase", label: "Read 2 of 2", index: 2, total: 2, mode: "read" });
    expect(s.progress).toBeNull();
    expect(s.state).toBeNull();
    expect(s.samples).toHaveLength(0);
  });

  it("starts clean when a new job begins", () => {
    let s = apply(initialJob, started);
    s = apply(s, { kind: "log", stream: "app", line: "old" });
    s = apply(s, { kind: "finished", outcome });
    s = apply(s, started);
    expect(s.log).toHaveLength(0);
    expect(s.outcome).toBeNull();
  });

  it("caps the log", () => {
    let s = apply(initialJob, started);
    for (let i = 0; i < 1100; i++) s = apply(s, { kind: "log", stream: "stdout", line: String(i) });
    expect(s.log).toHaveLength(1000);
    expect(s.log[0].line).toBe("100");
    expect(s.log[999].line).toBe("1099");
  });

  it("adopts a snapshot taken by a reloaded window", () => {
    const s = jobReducer(initialJob, {
      type: "snapshot",
      at: 5000,
      snapshot: {
        running: true,
        job: { id: 3, kind: "flash", startedAtMs: 1 },
        phase: { label: "Flash", index: 1, total: 2, mode: "flash" },
        state: "WriteFlash",
        progress: { current: 5, max: 10 },
        log: [{ stream: "app", line: "hello" }],
        outcome: null,
      },
    });
    expect(s.running).toBe(true);
    expect(s.state).toBe("WriteFlash");
    expect(s.samples).toEqual([{ t: 5000, value: 5 }]);
  });
});

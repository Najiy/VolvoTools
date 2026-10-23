import type { PhaseMode } from "../types";

export interface Stage {
  id: string;
  label: string;
  /** Flasher state names that mean this stage is running. */
  states: string[];
}

const PREPARE: Stage[] = [
  { id: "sleep", label: "Quiet the car network", states: ["FallAsleep"] },
  { id: "boot", label: "Enter boot mode", states: ["OpenChannels"] },
  { id: "load", label: "Load the flasher program", states: ["LoadBootloader"] },
  { id: "start", label: "Start the flasher program", states: ["StartBootloader"] },
];

const FINISH: Stage = { id: "wake", label: "Wake the car network", states: ["WakeUp", "Done"] };

// The flasher reports a read as EraseFlash then WriteFlash, so both mean "reading" here.
const READ_STAGES: Stage[] = [
  ...PREPARE,
  { id: "read", label: "Read memory", states: ["EraseFlash", "WriteFlash", "ReadFlash"] },
  FINISH,
];

const FLASH_STAGES: Stage[] = [
  ...PREPARE,
  { id: "erase", label: "Erase", states: ["EraseFlash"] },
  { id: "write", label: "Write", states: ["WriteFlash"] },
  FINISH,
];

export function stagesFor(mode: PhaseMode): Stage[] {
  return mode === "flash" ? FLASH_STAGES : READ_STAGES;
}

/** Index of the stage that `state` belongs to, or -1 before the first state arrives. */
export function stageIndex(mode: PhaseMode, state: string | null): number {
  if (!state) return -1;
  return stagesFor(mode).findIndex((stage) => stage.states.includes(state));
}

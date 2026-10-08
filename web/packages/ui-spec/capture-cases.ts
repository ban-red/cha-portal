// The shared hold-Esc cases (capture-cases.json), typed. Kept out of `index.ts` so the portal's
// bundle doesn't pull the cases in.
import casesJson from "./capture-cases.json";

export type EscEvent = "down" | "repeat" | "up" | "blur" | "tick" | "uncapture";

export interface EscStepCase {
  /** Milliseconds from the start of the case. */
  t: number;
  ev: EscEvent;
  /** The pointer is captured and Esc reaches the page (default true); for down and repeat. */
  captured?: boolean;
  /** The Esc key event this step sends to the host (default none). */
  host?: "down" | "up";
  /** This step let go of the pointer (default false). */
  release?: boolean;
  /** The hint's progress after this step (default hidden). */
  hint?: number;
}

export interface EscCase {
  name: string;
  steps: EscStepCase[];
}

const file = casesJson as unknown as {
  timing: { release_hold_ms: number; release_hint_ms: number };
  cases: EscCase[];
};

export const ESC_CASES = file.cases;
/** The timing the cases were worked out with, written in the file and not read from the spec. */
export const ESC_TIMING = file.timing;

import { atom } from "nanostores";

/**
 * How the in-call surface is presented to the user. Independent of
 * the call lifecycle in `$callState` so the user's last chosen
 * presentation outlives the connect/reconnect cycle.
 *
 * - `docked`: 400 px panel on the right edge (legacy default;
 *   leaves the channel timeline interactive on desktop).
 * - `floating`: small draggable window the user can park anywhere
 *   on screen. Allows the channel pane to take the full width
 *   while the call stays visible.
 * - `minimized`: collapsed to a chip in the corner; only the
 *   active speaker's name + the hang-up button. Useful when the
 *   user just wants to keep listening while typing in another
 *   channel.
 * - `pip`: native Picture-in-Picture (the browser draws the
 *   active speaker's video outside the page). The page itself
 *   continues to render the dock so the user can step out of PIP
 *   without losing controls — when PIP exits, the mode falls back
 *   to the previous one.
 */
export type CallUiMode = "docked" | "floating" | "minimized" | "pip";

const STORAGE_KEY = "waddle:call-ui-mode";
const POSITION_KEY = "waddle:call-floating-position";

function readInitialMode(): CallUiMode {
  if (typeof window === "undefined") return "docked";
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (raw === "docked" || raw === "floating" || raw === "minimized") {
      return raw;
    }
  } catch {
    // localStorage may throw in private modes; fall through to default.
  }
  return "docked";
}

export const $callUiMode = atom<CallUiMode>(readInitialMode());

$callUiMode.subscribe((mode) => {
  if (typeof window === "undefined") return;
  // Don't persist `pip` — when the user exits PIP we restore
  // the previously-chosen non-pip mode, not "next page load
  // starts in PIP" (which would fail because PIP requires a
  // user gesture).
  if (mode === "pip") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // Ignore — persistence is best-effort.
  }
});

/**
 * Saved floating-window position (top-left corner) in viewport
 * coordinates. Persisted to survive page reloads so a user who
 * parked the window in the bottom-right gets it back there next
 * call. Defaults to a sensible top-right anchor when no position
 * is saved.
 */
type FloatingPosition = { x: number; y: number };

function readInitialPosition(): FloatingPosition | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(POSITION_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (
      typeof parsed === "object" &&
      parsed !== null &&
      "x" in parsed &&
      "y" in parsed &&
      typeof parsed.x === "number" &&
      typeof parsed.y === "number"
    ) {
      return { x: parsed.x, y: parsed.y };
    }
  } catch {
    // Ignore parse failures; fall through to default position.
  }
  return null;
}

export const $callFloatingPosition = atom<FloatingPosition | null>(
  readInitialPosition(),
);

$callFloatingPosition.subscribe((pos) => {
  if (typeof window === "undefined") return;
  try {
    if (pos) {
      window.localStorage.setItem(POSITION_KEY, JSON.stringify(pos));
    } else {
      window.localStorage.removeItem(POSITION_KEY);
    }
  } catch {
    // Persistence is best-effort.
  }
});

/**
 * Mode the user was in before entering PIP, so exiting PIP can
 * restore the right surface (docked vs floating vs minimized).
 * Held in module scope rather than persisted because it's
 * meaningful only within a single PIP session.
 */
let preferredNonPipMode: Exclude<CallUiMode, "pip"> = "docked";

export function rememberNonPipMode(mode: CallUiMode): void {
  if (mode !== "pip") {
    preferredNonPipMode = mode;
  }
}

export function nonPipFallbackMode(): Exclude<CallUiMode, "pip"> {
  return preferredNonPipMode;
}

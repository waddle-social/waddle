import { atom } from "nanostores";

/**
 * How the in-call surface is presented to the user. Independent of
 * the call lifecycle in `$callState` so the user's last chosen
 * presentation outlives the connect/reconnect cycle.
 *
 * - `split`: the call renders inline above the channel's message lane
 *   so the chat is always visible beneath. A drag handle between the
 *   two regions resizes them; the position is persisted per room in
 *   `use-split-resize`.
 * - `expanded`: the call fills the entire chat content pane (where
 *   the channel chrome + timeline + composer normally live). The
 *   surrounding app shell (waddles rail, channel list, thread panel)
 *   stays visible so the user can still navigate; this is NOT the
 *   browser's native fullscreen.
 * - `immersive`: the call stage fills the viewport edge-to-edge and
 *   hides the surrounding app chrome. Native browser fullscreen can
 *   be layered on top of this mode, but leaving browser fullscreen
 *   returns to `expanded`.
 *
 * Anything that used to be `docked`, `floating`, `minimized`, or
 * `pip` is gone — see PR #743 for the rationale. The chat must never
 * be hidden behind the call, and the call must never escape the
 * channel that owns it.
 */
export type CallUiMode = "split" | "expanded" | "immersive";

const STORAGE_KEY = "waddle:call-ui-mode";

function readInitialMode(): CallUiMode {
  if (typeof window === "undefined") return "split";
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (raw === "split" || raw === "expanded" || raw === "immersive") {
      return raw;
    }
  } catch {
    // localStorage may throw in private modes; fall through to default.
  }
  return "split";
}

export const $callUiMode = atom<CallUiMode>(readInitialMode());

export function nextCallUiMode(mode: CallUiMode): CallUiMode {
  if (mode === "split") return "expanded";
  if (mode === "expanded") return "immersive";
  return "expanded";
}

export function callUiModeAfterFullscreenExit(mode: CallUiMode): CallUiMode {
  return mode === "immersive" ? "expanded" : mode;
}

export function callUiModeAfterSurfaceEscape(mode: CallUiMode): CallUiMode {
  return mode === "immersive" ? "expanded" : "split";
}

export function shouldExitNativeFullscreenForModeChange(
  currentMode: CallUiMode,
  nextMode: CallUiMode,
  nativeFullscreenActive: boolean,
): boolean {
  return currentMode === "immersive" && nextMode !== "immersive" && nativeFullscreenActive;
}

export function resetCallUiModeAfterCallEnd(): CallUiMode {
  return "split";
}

$callUiMode.subscribe((mode) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // Ignore — persistence is best-effort.
  }
});

// Debug-only scroll-anchor instrumentation (issue #677).
//
// Enable by setting `localStorage.setItem("WADDLE_DEBUG_SCROLL", "1")` in
// the browser before reproducing. Logs go to `console.debug` with the
// `[scroll-debug]` prefix and include monotonic timestamps relative to
// the first log of the session.
//
// Drop this file (and its imports) before shipping.

const PREFIX = "[scroll-debug]";

let enabledMemo: boolean | null = null;
let originMs: number | null = null;

function readEnabled(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage?.getItem("WADDLE_DEBUG_SCROLL") === "1";
  } catch {
    return false;
  }
}

function debugScrollEnabled(): boolean {
  if (enabledMemo === null) enabledMemo = readEnabled();
  return enabledMemo;
}

function elapsed(): number {
  const now = typeof performance !== "undefined" ? performance.now() : Date.now();
  if (originMs === null) originMs = now;
  return Math.round(now - originMs);
}

export function debugScroll(event: string, fields?: Record<string, unknown>): void {
  if (!debugScrollEnabled()) return;
  const t = elapsed();
  if (fields && Object.keys(fields).length > 0) {
    console.debug(`${PREFIX} +${t}ms ${event}`, fields);
  } else {
    console.debug(`${PREFIX} +${t}ms ${event}`);
  }
}

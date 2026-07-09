import type { ErrorKind } from "./error-classification";
import { reportError } from "./events";
import { observeTelemetry } from "./runtime";

// --- Global (window-level) error capture -----------------------------
//
// Faro's stock errors instrumentation is deliberately stripped in
// `getPrivacySafeWebInstrumentations()` because it ships raw messages
// and stacks. These handlers restore global capture but funnel every
// error through the sanitizing `reportError()` path instead.

const GLOBAL_ERROR_DEDUPE_WINDOW_MS = 5_000;
// Guard is per-window (not a process-lifetime latch) so a swapped
// `window` — a fresh test stub, an HMR-recreated document — gets its
// own listeners instead of a silent no-op.
let globalErrorsInstalledOn: unknown = null;
let reportingGlobalError = false;
let lastGlobalErrorKey = "";
let lastGlobalErrorAtMs = 0;

/**
 * Install window `error` + `unhandledrejection` capture. Idempotent
 * per window instance and a no-op outside the browser. Called once
 * from {@link initTelemetry}; `reportError` already no-ops without
 * Faro, so installing before or without a collector is harmless.
 */
export function installGlobalErrorTelemetry(): void {
  observeTelemetry(() => {
    if (typeof window === "undefined" || typeof window.addEventListener !== "function") return;
    if (globalErrorsInstalledOn === window) return;
    globalErrorsInstalledOn = window;

    window.addEventListener("error", (event) => {
      handleWindowErrorEvent(event);
    });
    window.addEventListener("unhandledrejection", (event) => {
      handleUnhandledRejectionEvent(event);
    });
  });
}

/** Exported for tests and for {@link installGlobalErrorTelemetry}. */
export function handleWindowErrorEvent(event: { error?: unknown; message?: unknown }): void {
  observeTelemetry(() => {
    const error = event.error
      ?? new Error(typeof event.message === "string" && event.message ? event.message : "window-error");
    reportGlobalError("window-error", error);
  });
}

/** Exported for tests and for {@link installGlobalErrorTelemetry}. */
export function handleUnhandledRejectionEvent(event: { reason?: unknown }): void {
  observeTelemetry(() => {
    reportGlobalError("unhandled-rejection", event.reason ?? new Error("unhandled-rejection"));
  });
}

/**
 * Report a Vue render/lifecycle error caught by `app.config.errorHandler`
 * (installed in `src/vue-app.ts`). Shares the loop guard + flood dedupe
 * with the window-level handlers.
 */
export function reportVueError(error: unknown, componentName?: string, info?: string): void {
  observeTelemetry(() => {
    reportGlobalError("vue-render-error", error, {
      ...(componentName ? { component: componentName } : {}),
      ...(info ? { detail: info } : {}),
    });
  });
}

function reportGlobalError(
  kind: ErrorKind,
  error: unknown,
  context: Record<string, unknown> = {},
): void {
  // Loop guard: an error thrown while reporting (inside Faro or our
  // sanitizers) would re-enter via the same global handlers forever.
  if (reportingGlobalError) return;
  reportingGlobalError = true;
  try {
    const message = error instanceof Error ? error.message : String(error);
    const key = `${kind}:${message}`;
    const now = Date.now();
    if (key === lastGlobalErrorKey && now - lastGlobalErrorAtMs < GLOBAL_ERROR_DEDUPE_WINDOW_MS) {
      return;
    }
    lastGlobalErrorKey = key;
    lastGlobalErrorAtMs = now;
    reportError(kind, error, { recoverable: false, ...context });
  } catch {
    // Never let telemetry take the page down (or recurse).
  } finally {
    reportingGlobalError = false;
  }
}


export function resetGlobalErrorDedupeForTesting(): void {
  reportingGlobalError = false;
  lastGlobalErrorKey = "";
  lastGlobalErrorAtMs = 0;
}

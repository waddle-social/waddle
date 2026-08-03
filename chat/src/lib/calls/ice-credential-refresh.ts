import type { IceServerBundle } from "./ice-servers";
import { browserTimerClock, type TimerClock } from "./timer-clock";

const REFRESH_SKEW_MS = 60_000;
const RETRY_MS = 30_000;

export type IceCredentialRefresher = {
  start(earliestExpiryMs: number | null): void;
  refreshNow(): Promise<void>;
  stop(): void;
};

/**
 * Own the call-scoped XEP-0215 refresh timer. The caller supplies the room
 * generation guard, so a fetch that outlives disconnect can neither replace
 * the next call's configuration nor emit telemetry for it.
 */
export function createIceCredentialRefresher(options: {
  refresh: () => Promise<IceServerBundle>;
  apply: (bundle: IceServerBundle) => void;
  isCurrent: () => boolean;
  onRefreshed: () => void;
  onExpired: () => void;
  clock?: TimerClock;
}): IceCredentialRefresher {
  const clock = options.clock ?? browserTimerClock;
  let expiryMs: number | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let refreshInFlight: Promise<void> | null = null;
  let stopped = true;
  let expiryReported = false;

  function clearTimer(): void {
    if (timer === null) return;
    clock.clearTimeout(timer);
    timer = null;
  }

  function reportExpiryIfNeeded(): void {
    if (expiryMs === null || expiryMs > clock.now() || expiryReported) return;
    expiryReported = true;
    options.onExpired();
  }

  function schedule(retryWhenDue: boolean): void {
    clearTimer();
    if (stopped || !options.isCurrent() || expiryMs === null) return;
    const untilExpiryMs = expiryMs - clock.now();
    const untilRefreshMs = untilExpiryMs - REFRESH_SKEW_MS;
    const delayMs = untilRefreshMs > 0
      ? untilRefreshMs
      : retryWhenDue
        ? untilExpiryMs > 0
          ? Math.min(RETRY_MS, untilExpiryMs)
          : RETRY_MS
        : 0;
    timer = clock.setTimeout(() => {
      timer = null;
      reportExpiryIfNeeded();
      void refreshNow();
    }, delayMs);
  }

  async function refreshNow(): Promise<void> {
    if (stopped || !options.isCurrent()) return;
    if (refreshInFlight) return refreshInFlight;
    refreshInFlight = (async () => {
      let bundle: IceServerBundle | null = null;
      try {
        bundle = await options.refresh();
      } catch {
        // The existing credentials remain the best available configuration;
        // retry below while the current call generation is still alive.
      }
      if (stopped || !options.isCurrent()) return;
      const now = clock.now();
      const usable = bundle !== null
        && bundle.servers.length > 0
        && (bundle.earliestExpiryMs === null || bundle.earliestExpiryMs > now);
      if (usable && bundle) {
        options.apply(bundle);
        expiryMs = bundle.earliestExpiryMs;
        expiryReported = false;
        options.onRefreshed();
      }
      reportExpiryIfNeeded();
      schedule(true);
    })().finally(() => {
      refreshInFlight = null;
    });
    return refreshInFlight;
  }

  return {
    start(earliestExpiryMs) {
      stopped = false;
      expiryMs = earliestExpiryMs;
      expiryReported = false;
      reportExpiryIfNeeded();
      schedule(false);
    },
    refreshNow,
    stop() {
      stopped = true;
      expiryMs = null;
      expiryReported = false;
      clearTimer();
    },
  };
}

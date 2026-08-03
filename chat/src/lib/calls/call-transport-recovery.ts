import { browserTimerClock, type TimerClock } from "./timer-clock";

export const CALL_TRANSPORT_RECOVERY_GRACE_MS = 60_000;

export type CallTransportRecovery = {
  onTransportLost(): "deferred" | "not-deferred";
  onTransportReady(): void;
  dispose(): void;
};

/**
 * Keep established media alive while its XMPP control plane has a bounded
 * chance to recover. Call activity is checked again at expiry so an obsolete
 * timer cannot tear down a call that already ended locally.
 */
export function createCallTransportRecovery(options: {
  isCallMediaActive(): boolean;
  teardown(): void;
  clock?: TimerClock;
}): CallTransportRecovery {
  const clock = options.clock ?? browserTimerClock;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function cancel(): void {
    if (timer !== null) clock.clearTimeout(timer);
    timer = null;
  }

  return {
    onTransportLost() {
      if (!options.isCallMediaActive()) return "not-deferred";
      if (timer === null) {
        timer = clock.setTimeout(() => {
          timer = null;
          if (options.isCallMediaActive()) options.teardown();
        }, CALL_TRANSPORT_RECOVERY_GRACE_MS);
      }
      return "deferred";
    },
    onTransportReady: cancel,
    dispose: cancel,
  };
}

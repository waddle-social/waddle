import { sameMicAudioProcessing, type MicAudioProcessing } from "./mic-audio-processing";

/**
 * Map the verified audio-processing state of the local mic to the flat,
 * string-valued attribute record we beacon to Grafana Faro (issue #913).
 *
 * The shape is deliberately minimal and PII-free: `kind` plus, when a mic
 * is publishing, the per-constraint tri-state (`on`/`off`/`unknown`). No
 * device IDs, no device labels, no JIDs — only the verified processing
 * state. Coarse browser/platform context rides along for free in Faro's
 * own event meta, so it is not duplicated here.
 */
export function callAudioProcessingEventAttributes(
  state: MicAudioProcessing,
): Record<string, string> {
  if (state.kind === "no-mic") return { kind: "no-mic" };
  return {
    kind: "active",
    noise_suppression: state.noiseSuppression,
    echo_cancellation: state.echoCancellation,
    auto_gain_control: state.autoGainControl,
  };
}

/** Stateful, per-call de-duplicating beacon over the verified mic state. */
export type CallAudioProcessingBeacon = {
  /** Report `state` unless it is value-equal to the last reported one. */
  observe(state: MicAudioProcessing): void;
  /** Forget the last reported state so the next call re-arms from scratch. */
  reset(): void;
};

/**
 * Wrap a `report` sink with the same value-dedup the #911 store uses, so a
 * given verified state beacons at most once per call: redundant recomputes
 * (the initial publish emits twice, mid-call restarts that change nothing)
 * collapse to a single beacon. `reset()` is called when a call ends so the
 * next call starts fresh.
 */
export function createCallAudioProcessingBeacon(
  report: (state: MicAudioProcessing) => void,
): CallAudioProcessingBeacon {
  let last: MicAudioProcessing | null = null;
  return {
    observe(state) {
      if (last && sameMicAudioProcessing(last, state)) return;
      last = state;
      report(state);
    },
    reset() {
      last = null;
    },
  };
}

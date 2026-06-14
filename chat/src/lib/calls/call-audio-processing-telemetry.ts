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
  /** Report `state` unless an equal state already beaconed this call. */
  observe(state: MicAudioProcessing): void;
  /** Forget the states seen this call so the next call re-arms from scratch. */
  reset(): void;
};

/**
 * Wrap a `report` sink so each *distinct* verified state beacons at most once
 * per call: redundant recomputes (the initial publish emits twice, mid-call
 * restarts that change nothing) collapse to a single beacon, and a state that
 * recurs after the mic cycles away and back — e.g. `active` → `no-mic` →
 * `active` on a mute/unmute or device reconnect — is not beaconed a second
 * time. Only genuinely new states for this call fire. `reset()` is called
 * when a call ends so the next call starts with an empty seen-set.
 *
 * The seen-set is bounded and tiny (one `no-mic` plus the handful of reachable
 * tri-state combinations), so a linear scan with the same value-equality the
 * #911 store uses is both simplest and correct.
 */
export function createCallAudioProcessingBeacon(
  report: (state: MicAudioProcessing) => void,
): CallAudioProcessingBeacon {
  let seen: MicAudioProcessing[] = [];
  return {
    observe(state) {
      if (seen.some((prior) => sameMicAudioProcessing(prior, state))) return;
      seen.push(state);
      report(state);
    },
    reset() {
      seen = [];
    },
  };
}

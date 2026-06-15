import {
  sameAiNoiseFilter,
  type AiNoiseFilterState,
} from "./ai-noise-filter/mic-ai-noise-filter";
import { sameMicAudioProcessing, type MicAudioProcessing } from "./mic-audio-processing";

/**
 * The full verified audio-processing state of the local mic for one beacon:
 * the browser-native constraint trio (read from `getSettings()`) plus the AI
 * noise filter (read from the attached processor's name). They come from two
 * different truth sources but are reported as one event so a fleet query sees
 * the whole picture — which layer is actually doing the cancellation.
 */
export type VerifiedCallAudioProcessing = {
  processing: MicAudioProcessing;
  aiNoiseFilter: AiNoiseFilterState;
};

/** The model id actually running, or "off" — never PII. */
function aiNoiseFilterAttribute(state: AiNoiseFilterState): string {
  return state.kind === "active" && state.model !== null ? state.model : "off";
}

/**
 * Map the verified state to the flat, string-valued attribute record we
 * beacon to Grafana Faro (issues #913 / #914).
 *
 * Deliberately minimal and PII-free: `kind`, plus (when a mic is publishing)
 * the per-constraint tri-state, plus `ai_noise_filter` (the active model id or
 * `off`). No device IDs, labels, or JIDs. Coarse browser/platform context
 * rides along for free in Faro's own event meta.
 */
export function callAudioProcessingEventAttributes(
  state: VerifiedCallAudioProcessing,
): Record<string, string> {
  const ai_noise_filter = aiNoiseFilterAttribute(state.aiNoiseFilter);
  if (state.processing.kind === "no-mic") return { kind: "no-mic", ai_noise_filter };
  return {
    kind: "active",
    noise_suppression: state.processing.noiseSuppression,
    echo_cancellation: state.processing.echoCancellation,
    auto_gain_control: state.processing.autoGainControl,
    ai_noise_filter,
  };
}

/** Stateful, per-call de-duplicating beacon over the verified state. */
export type CallAudioProcessingBeacon = {
  /** Report `state` unless an equal state already beaconed this call. */
  observe(state: VerifiedCallAudioProcessing): void;
  /** Forget the states seen this call so the next call re-arms from scratch. */
  reset(): void;
};

/** Value-equality of two snapshots — re-beacons when either layer changes. */
function sameVerifiedCallAudioProcessing(
  a: VerifiedCallAudioProcessing,
  b: VerifiedCallAudioProcessing,
): boolean {
  return (
    sameMicAudioProcessing(a.processing, b.processing) &&
    sameAiNoiseFilter(a.aiNoiseFilter, b.aiNoiseFilter)
  );
}

/**
 * Wrap a `report` sink so each *distinct* verified state beacons at most once
 * per call: redundant recomputes collapse to a single beacon, and a state that
 * recurs after the mic cycles away and back (active → no-mic → active on a
 * mute/unmute or device reconnect) is not beaconed a second time. A change in
 * either the constraint trio or the AI filter counts as a new state. `reset()`
 * is called when a call ends so the next call starts with an empty seen-set.
 *
 * The seen-set is bounded and tiny, so a linear scan with value-equality is
 * both simplest and correct.
 */
export function createCallAudioProcessingBeacon(
  report: (state: VerifiedCallAudioProcessing) => void,
): CallAudioProcessingBeacon {
  let seen: VerifiedCallAudioProcessing[] = [];
  return {
    observe(state) {
      if (seen.some((prior) => sameVerifiedCallAudioProcessing(prior, state))) return;
      seen.push(state);
      report(state);
    },
    reset() {
      seen = [];
    },
  };
}

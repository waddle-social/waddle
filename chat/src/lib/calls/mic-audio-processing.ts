/**
 * Verification of the browser-native audio processing the call engine
 * *requests* on every connect (`audioCaptureDefaults` in `engine.ts`:
 * `noiseSuppression`, `echoCancellation`, `autoGainControl`).
 *
 * Requesting a constraint and the browser *applying* it are different
 * things: some capture devices (notably certain Bluetooth headsets)
 * refuse noise suppression, and some browsers omit the field from
 * `MediaStreamTrack.getSettings()` entirely even while it is active.
 * So we read the live track's *applied* settings and model three
 * honest states per constraint — never claiming "on" we cannot confirm,
 * never crying "off" for a browser that simply stays quiet.
 *
 * This module is pure: it maps a `MediaTrackSettings` snapshot to the
 * typed state and derives the UI rows. The engine owns the "is there a
 * live mic track at all?" decision (the `no-mic` case); everything here
 * is a total function of its inputs and is unit-tested in isolation.
 */

/**
 * The applied state of a single audio-processing constraint:
 *  - `on`      — `getSettings()` reported `true` (requested *and* applied).
 *  - `off`     — reported `false` (requested, but the device/browser
 *                refused it — a genuine degradation worth surfacing).
 *  - `unknown` — the field was absent from `getSettings()`; the browser
 *                doesn't report it, which usually means "probably applied
 *                but unconfirmable", NOT "off".
 */
export type ConstraintState = "on" | "off" | "unknown";

/**
 * The applied audio-processing state of the local microphone.
 *
 * Discriminated union rather than a flat record so "no microphone is
 * publishing" is unrepresentable as a per-constraint value: the UI must
 * handle the `no-mic` case before it can read any constraint, and the
 * three constraints can never disagree about whether a mic exists.
 */
export type MicAudioProcessing =
  | { kind: "no-mic" }
  | {
      kind: "active";
      noiseSuppression: ConstraintState;
      echoCancellation: ConstraintState;
      autoGainControl: ConstraintState;
    };

/**
 * Map one `getSettings()` value to a tri-state.
 *
 * Most constraints report a plain `boolean`. `echoCancellation` is the
 * exception: some browsers report it as an *echo-cancellation-mode*
 * string (`"all"` / `"remote-only"`) instead of a boolean — a present
 * mode means the processing is active, so a non-empty string reads as
 * `on`. An absent field stays `unknown` (the browser doesn't report it).
 */
export function constraintState(value: boolean | string | undefined): ConstraintState {
  if (value === true) return "on";
  if (value === false) return "off";
  if (typeof value === "string") return value.length > 0 ? "on" : "off";
  return "unknown";
}

/**
 * Read the applied audio-processing trio from a live mic track's
 * settings. Caller guarantees the track is actually capturing; the
 * `no-mic` case is decided upstream (in the engine) where the track's
 * existence and `readyState` are known.
 */
export function activeMicAudioProcessing(
  settings: MediaTrackSettings,
): Extract<MicAudioProcessing, { kind: "active" }> {
  return {
    kind: "active",
    noiseSuppression: constraintState(settings.noiseSuppression),
    echoCancellation: constraintState(settings.echoCancellation),
    autoGainControl: constraintState(settings.autoGainControl),
  };
}

/**
 * Visual weight for a constraint state, matching meaning to prominence:
 *  - `on`   — calm positive; the requested processing is confirmed.
 *  - `warn` — the browser told us it is `off`; a real degradation.
 *  - `muted` — `unknown`; greyed, since we can't confirm either way.
 */
type AudioProcessingTone = "on" | "warn" | "muted";

type AudioProcessingRowKey =
  | "noiseSuppression"
  | "echoCancellation"
  | "autoGainControl";

/** One presentational row for the call-settings indicator. */
type AudioProcessingRow = {
  key: AudioProcessingRowKey;
  label: string;
  state: ConstraintState;
  stateLabel: string;
  tone: AudioProcessingTone;
  /** Caption shown under non-`on` rows; `null` when the state is `on`. */
  detail: string | null;
};

const ROW_LABELS: Readonly<Record<AudioProcessingRowKey, string>> = {
  noiseSuppression: "Noise cancellation",
  echoCancellation: "Echo cancellation",
  autoGainControl: "Automatic gain control",
};

const STATE_LABELS: Readonly<Record<ConstraintState, string>> = {
  on: "On",
  off: "Off",
  unknown: "Not reported",
};

const UNKNOWN_DETAIL = "Your browser doesn't report this setting.";
const OFF_DETAIL = "Your microphone or browser turned this off.";

function toneFor(state: ConstraintState): AudioProcessingTone {
  if (state === "on") return "on";
  if (state === "off") return "warn";
  return "muted";
}

function detailFor(state: ConstraintState): string | null {
  if (state === "off") return OFF_DETAIL;
  if (state === "unknown") return UNKNOWN_DETAIL;
  return null;
}

/** Render order: noise cancellation first (the headline), then the rest. */
const ROW_ORDER: readonly AudioProcessingRowKey[] = [
  "noiseSuppression",
  "echoCancellation",
  "autoGainControl",
];

/** Derive the indicator's display rows from the active trio. */
export function audioProcessingRows(
  state: Extract<MicAudioProcessing, { kind: "active" }>,
): AudioProcessingRow[] {
  return ROW_ORDER.map((key) => {
    const value = state[key];
    return {
      key,
      label: ROW_LABELS[key],
      state: value,
      stateLabel: STATE_LABELS[value],
      tone: toneFor(value),
      detail: detailFor(value),
    };
  });
}

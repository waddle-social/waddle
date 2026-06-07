import { atom } from "nanostores";

/**
 * The local participant's call connection health, as a domain union
 * decoupled from LiveKit's `ConnectionQuality` enum. The engine (the
 * boundary to LiveKit) translates the SDK enum into these values via
 * `mapLiveKitConnectionQuality`, so this module — and the UI that reads
 * it — never imports `livekit-client`, stays trivially unit-testable,
 * and is insulated from SDK enum churn.
 */
export type CallConnectionQuality =
  | "excellent"
  | "good"
  | "poor"
  | "lost"
  | "unknown";

/**
 * The room transport phase, distilled from LiveKit's `ConnectionState`.
 * `reconnecting` covers both ICE and signal reconnection — either way
 * the indicator should say so. Distinct from quality because a transport
 * that is re-establishing should override whatever the last quality
 * sample said.
 */
export type CallConnectionPhase = "connected" | "reconnecting" | "disconnected";

/** Visual tone of the indicator, mapped to the badge color tokens. */
type ConnectionChipTone = "neutral" | "warn" | "danger";

/**
 * Rendering model for the ambient signal-bars chip. `bars` is how many
 * of the three segments are filled; `label` is `null` when the
 * connection is healthy (bars alone, no nagging text) and a short string
 * when it has degraded.
 */
export type ConnectionChip = {
  bars: 0 | 1 | 2 | 3;
  tone: ConnectionChipTone;
  label: string | null;
};

/**
 * Live inputs for the self-quality indicator. Set by the engine
 * subscription in `use-call-engine`; read by `CallConnectionIndicator`.
 * Defaults describe "no call / nothing known yet" so the indicator shows
 * quiet measuring bars until the first real quality sample arrives.
 */
export const $callConnectionQuality = atom<CallConnectionQuality>("unknown");
export const $callConnectionPhase = atom<CallConnectionPhase>("disconnected");

export function setCallConnectionQuality(quality: CallConnectionQuality): void {
  $callConnectionQuality.set(quality);
}

export function setCallConnectionPhase(phase: CallConnectionPhase): void {
  $callConnectionPhase.set(phase);
}

/**
 * Reset to the "no call" baseline. Called on engine disconnect so a
 * stale `poor`/`reconnecting` from the call that just ended cannot bleed
 * into the next call's first render.
 */
export function resetCallConnectionQuality(): void {
  $callConnectionQuality.set("unknown");
  $callConnectionPhase.set("disconnected");
}

/**
 * Pure mapping from (quality, phase) to the chip rendering model. Always
 * returns a chip so the indicator holds a stable footprint in the call
 * bar — `unknown` renders quiet, empty "measuring" bars rather than
 * vanishing, which would reflow the centered control row when the first
 * sample lands.
 *
 * A reconnecting transport overrides the last quality sample with
 * "Reconnecting…" — the bars are meaningless while the path is down.
 * `lost` is a per-participant quality score independent of the transport
 * phase, so it gets its own "Connection lost" label rather than falsely
 * claiming a reconnection is underway.
 */
export function qualityToChip(
  quality: CallConnectionQuality,
  phase: CallConnectionPhase,
): ConnectionChip {
  if (phase === "reconnecting") {
    return { bars: 0, tone: "danger", label: "Reconnecting…" };
  }
  switch (quality) {
    case "excellent":
      return { bars: 3, tone: "neutral", label: null };
    case "good":
      return { bars: 2, tone: "neutral", label: null };
    case "poor":
      return { bars: 1, tone: "warn", label: "Poor connection" };
    case "lost":
      return { bars: 0, tone: "danger", label: "Connection lost" };
    case "unknown":
      return { bars: 0, tone: "neutral", label: null };
  }
}

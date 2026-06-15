/**
 * The *verified* state of the AI noise filter on the local mic — the honest
 * companion to `mic-audio-processing.ts`.
 *
 * The #911 indicator reads the capture source's `getSettings()`, which is
 * blind to track processors, so a running WASM filter never shows up there.
 * This module instead derives "which AI model is actually live?" from the
 * attached processor's `name` (`localAudioTrack.getProcessor()?.name`): if
 * `setProcessor`'s `init` ever rejects, LiveKit keeps the previous processor,
 * so we never falsely claim a model is active.
 *
 * Pure: a total function of the processor name. The engine owns the "is there
 * a live mic at all?" decision (the `no-mic` case), exactly as it does for the
 * browser-constraint trio.
 */

import { modelIdFromProcessorName, type NoiseModelId } from "./model-id";
import { noiseModelMeta } from "./model-metadata";

/**
 * Discriminated union so "no microphone is publishing" is unrepresentable as
 * a model value. `{ kind: "active", model: null }` means a mic is live but no
 * AI filter is attached — i.e. honestly "Off", distinct from "no mic".
 */
export type AiNoiseFilterState =
  | { kind: "no-mic" }
  | { kind: "active"; model: NoiseModelId | null };

/**
 * Map a live processor `name` to the active model. `null` model when nothing
 * is attached or the attached processor isn't one of ours. Caller guarantees
 * a mic is actually capturing; `no-mic` is decided upstream in the engine.
 */
export function activeAiNoiseFilter(
  processorName: string | undefined,
): Extract<AiNoiseFilterState, { kind: "active" }> {
  return { kind: "active", model: modelIdFromProcessorName(processorName) };
}

/** Structural equality, so the store can skip redundant notifications. */
export function sameAiNoiseFilter(a: AiNoiseFilterState, b: AiNoiseFilterState): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind !== "active" || b.kind !== "active") return true; // both no-mic
  return a.model === b.model;
}

/** Visual weight, matching the trio rows: positive when on, muted when off. */
type AiNoiseFilterTone = "on" | "muted";

/** One presentational row for the call-settings indicator. */
export type AiNoiseFilterRow = {
  label: string;
  stateLabel: string;
  tone: AiNoiseFilterTone;
  detail: string | null;
};

const ROW_LABEL = "AI noise filter";
const ACTIVE_DETAIL = "Running entirely in your browser.";

/** Derive the indicator row from the verified active state. */
export function aiNoiseFilterRow(
  state: Extract<AiNoiseFilterState, { kind: "active" }>,
): AiNoiseFilterRow {
  if (state.model === null) {
    return { label: ROW_LABEL, stateLabel: "Off", tone: "muted", detail: null };
  }
  return {
    label: ROW_LABEL,
    stateLabel: noiseModelMeta(state.model).label,
    tone: "on",
    detail: ACTIVE_DETAIL,
  };
}

import { atom } from "nanostores";
import {
  sameAiNoiseFilter,
  type AiNoiseFilterState,
} from "./ai-noise-filter/mic-ai-noise-filter";

/**
 * Verified AI-noise-filter state of the local microphone for the active call —
 * the sibling of `$micAudioProcessing`. The engine emits `aiNoiseFilterChanged`
 * on every transition that can change it (mic publish/unpublish, device switch,
 * mute/unmute, model selection) and `use-call-engine` mirrors it here; the
 * call-settings dialog reads it reactively. `no-mic` is the resting state.
 */
export const $micAiNoiseFilter = atom<AiNoiseFilterState>({ kind: "no-mic" });

export function setMicAiNoiseFilter(state: AiNoiseFilterState): void {
  if (sameAiNoiseFilter($micAiNoiseFilter.get(), state)) return;
  $micAiNoiseFilter.set(state);
}

/** Reset to `no-mic` — used when a call disconnects. */
export function resetMicAiNoiseFilter(): void {
  setMicAiNoiseFilter({ kind: "no-mic" });
}

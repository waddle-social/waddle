import { atom } from "nanostores";
import { sameMicAudioProcessing, type MicAudioProcessing } from "./mic-audio-processing";

/**
 * Applied audio-processing state of the local microphone for the active
 * call. The engine emits `micAudioProcessingChanged` on every transition
 * that can change it (mic publish / unpublish / device switch) and
 * `use-call-engine` mirrors it here; the call-settings dialog reads it
 * reactively. `no-mic` is the resting state — before a call, while a
 * listener has no mic, or after the call ends.
 */
export const $micAudioProcessing = atom<MicAudioProcessing>({ kind: "no-mic" });

export function setMicAudioProcessing(state: MicAudioProcessing): void {
  // Skip redundant notifications: a recompute often yields an unchanged
  // value (the initial publish emits twice — see sameMicAudioProcessing).
  if (sameMicAudioProcessing($micAudioProcessing.get(), state)) return;
  $micAudioProcessing.set(state);
}

/** Reset to `no-mic` — used when a call disconnects. */
export function resetMicAudioProcessing(): void {
  setMicAudioProcessing({ kind: "no-mic" });
}

import {
  type ResolvedCallDevicePreference,
  type AudioProcessingPrefs,
  setAiNoiseModel,
  setBackgroundEffectPref,
  setCamDevice,
  setAudioProcessingPrefs,
  setMicDevice,
  setSpeakerDevice,
} from "./device-prefs";
import { clearAiNoiseFilterError } from "./ai-noise-filter-error-state";
import { clearBackgroundEffectError } from "./background-effect-error-state";
import type { NoiseModelId } from "./ai-noise-filter/model-id";
import type { BackgroundEffect } from "./background-effect/effect-id";

export type CallDeviceKind = "mic" | "cam" | "speaker";

export type CallDeviceSelectionEngine = {
  setMicDevice(deviceId: string): Promise<ResolvedCallDevicePreference | null>;
  setCameraDevice(deviceId: string): Promise<ResolvedCallDevicePreference | null>;
  setSpeakerDevice(deviceId: string): Promise<ResolvedCallDevicePreference | null>;
};

export type CallAudioProcessingSelectionEngine = {
  setAudioProcessing(prefs: AudioProcessingPrefs): Promise<void>;
};

export async function applyCallDeviceSelection(
  kind: CallDeviceKind,
  deviceId: string | null,
  engine: CallDeviceSelectionEngine,
): Promise<void> {
  // Resolve exactly ONCE — inside the engine, which returns what it
  // actually applied. Persisting anything else lets the preference (and
  // any UI reading it) claim a device the live call is not capturing
  // from when the device list changes between two enumerations (#1621
  // review round 2). A null resolution means no live room (settings
  // outside a call) — persist the picker's intent verbatim; the picker
  // only offers enumerated devices.
  const requested = deviceId ?? "default";
  if (kind === "mic") {
    const applied = await engine.setMicDevice(requested);
    setMicDevice(applied ? applied.preferenceId : deviceId);
    return;
  }
  if (kind === "cam") {
    const applied = await engine.setCameraDevice(requested);
    setCamDevice(applied ? applied.preferenceId : deviceId);
    return;
  }
  const applied = await engine.setSpeakerDevice(requested);
  setSpeakerDevice(applied ? applied.preferenceId : deviceId);
}

export async function applyAudioProcessingSelection(
  prefs: AudioProcessingPrefs,
  engine: CallAudioProcessingSelectionEngine,
): Promise<void> {
  await engine.setAudioProcessing(prefs);
  setAudioProcessingPrefs(prefs);
}

export type CallAiNoiseSelectionEngine = {
  setAiNoiseModel(model: NoiseModelId | null): Promise<void>;
};

/**
 * Select the AI noise model (or null/off) for the active call: clear any stale
 * attach-failure notice (this is an explicit fresh attempt), apply it to the
 * engine, then persist the pref so the next call re-applies it. The engine
 * fails open on attach error, so this does not reject on a bad model.
 */
export async function applyAiNoiseModelSelection(
  model: NoiseModelId | null,
  engine: CallAiNoiseSelectionEngine,
): Promise<void> {
  clearAiNoiseFilterError();
  await engine.setAiNoiseModel(model);
  setAiNoiseModel(model);
}

export type CallBackgroundSelectionEngine = {
  setBackgroundEffect(effect: BackgroundEffect): Promise<void>;
};

/**
 * Select the camera background effect (off / blur / image) for the active call:
 * clear any stale attach-failure notice (this is an explicit fresh attempt),
 * apply it to the engine, then persist the pref so the next call re-applies it.
 * The engine fails open on attach error, so this does not reject on a bad effect.
 */
export async function applyBackgroundEffectSelection(
  effect: BackgroundEffect,
  engine: CallBackgroundSelectionEngine,
): Promise<void> {
  clearBackgroundEffectError();
  await engine.setBackgroundEffect(effect);
  setBackgroundEffectPref(effect);
}

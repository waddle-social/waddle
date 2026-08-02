import {
  resolveCallDevicePreference,
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
import { recordMediaIssue } from "./call-media-issues";

export type CallDeviceKind = "mic" | "cam" | "speaker";

export type CallDeviceSelectionEngine = {
  setMicDevice(deviceId: string): Promise<void>;
  setCameraDevice(deviceId: string): Promise<void>;
  setSpeakerDevice(deviceId: string): Promise<void>;
};

export type CallAudioProcessingSelectionEngine = {
  setAudioProcessing(prefs: AudioProcessingPrefs): Promise<void>;
};

export async function applyCallDeviceSelection(
  kind: CallDeviceKind,
  deviceId: string | null,
  engine: CallDeviceSelectionEngine,
): Promise<void> {
  const resolved = await resolveCallDevicePreference(kind, deviceId);
  if (resolved.missing && kind !== "speaker") {
    const error = new Error(`${kind} device is no longer available`);
    error.name = "NotFoundError";
    recordMediaIssue(kind, error);
  }
  if (kind === "mic") {
    await engine.setMicDevice(resolved.activeDeviceId);
    setMicDevice(resolved.preferenceId);
    return;
  }
  if (kind === "cam") {
    await engine.setCameraDevice(resolved.activeDeviceId);
    setCamDevice(resolved.preferenceId);
    return;
  }
  await engine.setSpeakerDevice(resolved.activeDeviceId);
  setSpeakerDevice(resolved.preferenceId);
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

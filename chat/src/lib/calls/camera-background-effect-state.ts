import { atom } from "nanostores";
import {
  sameCameraBackground,
  type CameraBackgroundState,
} from "./background-effect/camera-background";

/**
 * Verified camera background-effect state for the active call — the video
 * sibling of `$micAiNoiseFilter`. The engine emits `backgroundEffectChanged` on
 * every transition that can change it (camera publish/unpublish, device switch,
 * effect selection) and `use-call-engine` mirrors it here; the call-settings
 * dialog reads it reactively. `no-camera` is the resting state.
 */
export const $cameraBackground = atom<CameraBackgroundState>({ kind: "no-camera" });

export function setCameraBackground(state: CameraBackgroundState): void {
  if (sameCameraBackground($cameraBackground.get(), state)) return;
  $cameraBackground.set(state);
}

/** Reset to `no-camera` — used when a call disconnects. */
export function resetCameraBackground(): void {
  setCameraBackground({ kind: "no-camera" });
}

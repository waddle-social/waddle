/**
 * The *verified* background effect on the local camera — the video sibling of
 * the AI-noise filter's `mic-ai-noise-filter.ts`.
 *
 * The engine derives "which effect is actually live?" from the attached
 * processor's presence (`cameraTrack.getProcessor()?.name`) combined with the
 * effect it last successfully applied, so a `setProcessor` that silently fails
 * never makes the UI claim an effect is on. `no-camera` is the resting state
 * when nothing is publishing video.
 */

import { sameBackgroundEffect, type BackgroundEffect } from "./effect-id";

/**
 * Discriminated union so "no camera is publishing" is unrepresentable as an
 * effect value. `{ kind: "active", effect: { kind: "off" } }` means a camera is
 * live with no effect attached — honestly "Off", distinct from "no camera".
 */
export type CameraBackgroundState =
  | { kind: "no-camera" }
  | { kind: "active"; effect: BackgroundEffect };

/** Structural equality, so the store can skip redundant notifications. */
export function sameCameraBackground(a: CameraBackgroundState, b: CameraBackgroundState): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind !== "active" || b.kind !== "active") return true; // both no-camera
  return sameBackgroundEffect(a.effect, b.effect);
}

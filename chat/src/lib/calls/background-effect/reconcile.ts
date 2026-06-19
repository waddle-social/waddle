/**
 * The reconciler that keeps the local camera's attached background video
 * processor in sync with the user's desired effect.
 *
 * Mirrors the AI-noise-filter reconciler: a pure decision keyed on the live
 * processor's state, plus an idempotent runner the engine fires defensively on
 * every camera transition (publish / device change / explicit selection). The
 * library's `BackgroundProcessor` can switch between blur and image *in place*
 * to avoid visual artifacts, so the decision distinguishes a first `attach`
 * (no processor yet) from an in-place `switch`.
 */

import {
  backgroundEffectKey,
  sameBackgroundEffect,
  type ActiveBackgroundEffect,
  type BackgroundEffect,
} from "./effect-id";

/** The action the pure decision yields for the engine to perform. */
export type BackgroundEffectAction =
  | { type: "none" }
  | { type: "clear" }
  | { type: "attach"; effect: ActiveBackgroundEffect }
  | { type: "switch"; effect: ActiveBackgroundEffect };

/**
 * Decide what to do given the desired effect, the effect currently applied to
 * the live camera, and the set of effect keys that have already failed to
 * attach on *this* camera track. Pure.
 *
 * The guard makes an effect that just failed a no-op until the track is
 * replaced or the user re-selects (both reset the guard upstream), so the
 * defensive re-runs never become a retry loop — and it never leaves a *wrong*
 * effect running in place of the one the user picked.
 */
export function decideBackgroundEffectAction(
  desired: BackgroundEffect,
  current: BackgroundEffect,
  failed: ReadonlySet<string> = new Set(),
): BackgroundEffectAction {
  if (sameBackgroundEffect(desired, current)) return { type: "none" };
  if (desired.kind === "off") return { type: "clear" };
  // An active effect is desired.
  if (failed.has(backgroundEffectKey(desired))) {
    return current.kind === "off" ? { type: "none" } : { type: "clear" };
  }
  if (current.kind === "off") return { type: "attach", effect: desired };
  return { type: "switch", effect: desired };
}

/**
 * The live camera track the reconciler drives, reduced to the four operations
 * it needs. Backed by a `LocalVideoTrack` + the library processor in the
 * engine; by a fake in tests.
 */
export interface BackgroundProcessorTarget {
  /** The effect currently applied to the live camera. */
  currentEffect(): BackgroundEffect;
  /** Build a processor for `effect` and attach it (`localVideoTrack.setProcessor`). */
  attach(effect: ActiveBackgroundEffect): Promise<void>;
  /** Switch the attached processor to `effect` in place (`wrapper.switchTo`). */
  switch(effect: ActiveBackgroundEffect): Promise<void>;
  /** Remove the processor (`localVideoTrack.stopProcessor`). */
  clear(): Promise<void>;
}

/** What the reconcile run actually did — drives engine events. */
export type BackgroundEffectOutcome =
  | { action: "none" }
  | { action: "cleared" }
  | { action: "attached"; effect: ActiveBackgroundEffect }
  | { action: "switched"; effect: ActiveBackgroundEffect }
  | { action: "failed"; effect: ActiveBackgroundEffect; error: unknown };

/**
 * Perform the decided action against the target. On any failure building,
 * attaching, or switching the processor, arm the per-effect guard (so the next
 * defensive re-run is a no-op) and report it; the caller fails open to the raw
 * camera and surfaces a non-blocking notice. `failed` is mutated in place.
 */
export async function runBackgroundEffectReconcile(args: {
  target: BackgroundProcessorTarget;
  desired: BackgroundEffect;
  failed: Set<string>;
}): Promise<BackgroundEffectOutcome> {
  const { target, desired, failed } = args;
  const action = decideBackgroundEffectAction(desired, target.currentEffect(), failed);
  switch (action.type) {
    case "none":
      return { action: "none" };
    case "clear":
      await target.clear();
      return { action: "cleared" };
    case "attach":
      return applyOrFailOpen(target, failed, action.effect, () => target.attach(action.effect), "attached");
    case "switch":
      return applyOrFailOpen(target, failed, action.effect, () => target.switch(action.effect), "switched");
  }
}

/**
 * Run a build-and-apply step (attach or switch); on rejection arm the guard and
 * clear any processor left running so the camera falls back to raw, never to a
 * half-applied or wrong effect.
 */
async function applyOrFailOpen(
  target: BackgroundProcessorTarget,
  failed: Set<string>,
  effect: ActiveBackgroundEffect,
  apply: () => Promise<void>,
  ok: "attached" | "switched",
): Promise<BackgroundEffectOutcome> {
  try {
    await apply();
    return { action: ok, effect };
  } catch (error) {
    failed.add(backgroundEffectKey(effect));
    if (target.currentEffect().kind !== "off") {
      await target.clear().catch(() => undefined);
    }
    return { action: "failed", effect, error };
  }
}

/**
 * The reconciler that keeps the local mic's attached AI-noise processor in
 * sync with the user's selected model.
 *
 * LiveKit already re-runs an attached processor across `restartTrack` and
 * `switchActiveDevice` and destroys it on track stop, but the engine still
 * fires this defensively on every mic transition (publish / device change /
 * mute / unmute) plus on an explicit selection. So it MUST be idempotent:
 * the decision is keyed on the live `getProcessor()?.name`, and a model that
 * is already attached is left alone — re-attaching on every mute event would
 * tear down a working processor and glitch the outgoing audio.
 *
 * Generic over the processor type `P` so the pure logic is fully decoupled
 * from livekit-client: the engine instantiates `P` as a real `TrackProcessor`,
 * tests instantiate it as a fake.
 */

import { modelIdFromProcessorName, type NoiseModelId } from "./model-id";

/**
 * The live mic track the reconciler drives, reduced to the three operations
 * it needs. Backed by `LocalAudioTrack` in the engine; by a fake in tests.
 */
export interface ProcessorTarget<P> {
  /** Name of the currently attached processor, or undefined if none. */
  currentProcessorName(): string | undefined;
  /** Attach a processor (wraps `localAudioTrack.setProcessor`). */
  attach(processor: P): Promise<void>;
  /** Remove the current processor (wraps `localAudioTrack.stopProcessor`). */
  clear(): Promise<void>;
}

/** The action the pure decision yields for the orchestrator to perform. */
export type AiNoiseFilterAction =
  | { type: "none" }
  | { type: "stop" }
  | { type: "attach"; model: NoiseModelId };

/**
 * Decide what to do, given the desired model, the live processor name, and
 * the set of models that have already failed to attach on *this* track.
 *
 * Pure. The guard makes a model that just failed a no-op until the track is
 * replaced or the user re-selects (both reset the guard upstream), so the
 * defensive re-runs never become a retry loop.
 */
export function decideAiNoiseFilterAction(
  desired: NoiseModelId | null,
  currentName: string | undefined,
  failedModels: ReadonlySet<NoiseModelId>,
): AiNoiseFilterAction {
  const current = modelIdFromProcessorName(currentName);
  if (desired === current) return { type: "none" };
  if (desired === null) return { type: "stop" };
  // A model is desired and differs from what's attached.
  if (failedModels.has(desired)) {
    // Can't attach a model that just failed; never leave a model the user
    // didn't pick running in its place.
    return current === null ? { type: "none" } : { type: "stop" };
  }
  return { type: "attach", model: desired };
}

/** What the reconcile run actually did — drives engine events. */
export type ReconcileOutcome =
  | { action: "none" }
  | { action: "stopped" }
  | { action: "attached"; model: NoiseModelId }
  | { action: "failed"; model: NoiseModelId; error: unknown };

/**
 * Perform the decided action against the target. On any failure building or
 * attaching the processor, arm the per-model guard (so the next defensive
 * re-run is a no-op) and report it — the caller fails open to the raw mic and
 * surfaces a non-blocking notice. `failedModels` is mutated in place.
 */
export async function runAiNoiseFilterReconcile<P>(args: {
  target: ProcessorTarget<P>;
  desired: NoiseModelId | null;
  makeProcessor: (model: NoiseModelId) => Promise<P>;
  failedModels: Set<NoiseModelId>;
}): Promise<ReconcileOutcome> {
  const { target, desired, makeProcessor, failedModels } = args;
  const action = decideAiNoiseFilterAction(desired, target.currentProcessorName(), failedModels);
  switch (action.type) {
    case "none":
      return { action: "none" };
    case "stop":
      await target.clear();
      return { action: "stopped" };
    case "attach": {
      try {
        const processor = await makeProcessor(action.model);
        await target.attach(processor);
        return { action: "attached", model: action.model };
      } catch (error) {
        failedModels.add(action.model);
        return { action: "failed", model: action.model, error };
      }
    }
  }
}

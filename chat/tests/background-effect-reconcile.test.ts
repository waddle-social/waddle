import { describe, expect, mock, test } from "bun:test";
import {
  decideBackgroundEffectAction,
  runBackgroundEffectReconcile,
  type BackgroundProcessorTarget,
} from "../src/lib/calls/background-effect/reconcile";
import {
  BACKGROUND_OFF,
  backgroundEffectKey,
  type ActiveBackgroundEffect,
  type BackgroundEffect,
} from "../src/lib/calls/background-effect/effect-id";

/**
 * A fake processor target: holds the applied effect in memory and records the
 * operations the runner drives, with no livekit-client or MediaPipe in sight.
 */
function fakeTarget(opts: {
  initial?: BackgroundEffect;
  attach?: (effect: ActiveBackgroundEffect) => Promise<void>;
  switch?: (effect: ActiveBackgroundEffect) => Promise<void>;
}): BackgroundProcessorTarget & {
  calls: { attach: typeof attach; switch: typeof switchTo; clear: typeof clear };
} {
  let current: BackgroundEffect = opts.initial ?? BACKGROUND_OFF;
  const attach = mock(async (effect: ActiveBackgroundEffect) => {
    await opts.attach?.(effect);
    current = effect;
  });
  const switchTo = mock(async (effect: ActiveBackgroundEffect) => {
    await opts.switch?.(effect);
    current = effect;
  });
  const clear = mock(async () => {
    current = BACKGROUND_OFF;
  });
  return {
    currentEffect: () => current,
    attach,
    switch: switchTo,
    clear,
    calls: { attach, switch: switchTo, clear },
  };
}

describe("decideBackgroundEffectAction", () => {
  test("attaching an effect to a bare camera yields attach", () => {
    const action = decideBackgroundEffectAction({ kind: "blur" }, BACKGROUND_OFF);

    expect(action).toEqual({ type: "attach", effect: { kind: "blur" } });
  });

  test("re-selecting the already-applied effect is a no-op", () => {
    const action = decideBackgroundEffectAction({ kind: "blur" }, { kind: "blur" });

    expect(action).toEqual({ type: "none" });
  });

  test("turning the effect off clears the attached processor", () => {
    const current = { kind: "image", image: { source: "catalog", id: "office" } } as const;

    const action = decideBackgroundEffectAction(BACKGROUND_OFF, current);

    expect(action).toEqual({ type: "clear" });
  });

  test("changing between two live effects switches in place (not a fresh attach)", () => {
    const next = { kind: "image", image: { source: "catalog", id: "mountain" } } as const;

    const action = decideBackgroundEffectAction(next, { kind: "blur" });

    expect(action).toEqual({ type: "switch", effect: next });
  });

  test("re-selecting the same catalog image is a no-op (image equality)", () => {
    const image = { kind: "image", image: { source: "catalog", id: "office" } } as const;

    const action = decideBackgroundEffectAction(image, { ...image });

    expect(action).toEqual({ type: "none" });
  });

  test("an effect that just failed is not retried while nothing is attached", () => {
    const desired = { kind: "blur" } as const;
    const failed = new Set([backgroundEffectKey(desired)]);

    const action = decideBackgroundEffectAction(desired, BACKGROUND_OFF, failed);

    expect(action).toEqual({ type: "none" });
  });

  test("a failed effect clears the wrong one left running rather than retrying it", () => {
    const desired = { kind: "image", image: { source: "custom", ref: "u1" } } as const;
    const failed = new Set([backgroundEffectKey(desired)]);

    const action = decideBackgroundEffectAction(desired, { kind: "blur" }, failed);

    expect(action).toEqual({ type: "clear" });
  });
});

describe("runBackgroundEffectReconcile", () => {
  test("attaches the desired effect on a bare camera", async () => {
    const target = fakeTarget({});

    const outcome = await runBackgroundEffectReconcile({
      target,
      desired: { kind: "blur" },
      failed: new Set(),
    });

    expect(target.calls.attach).toHaveBeenCalledTimes(1);
    expect(outcome).toEqual({ action: "attached", effect: { kind: "blur" } });
  });

  test("clears the processor when the desired effect is off", async () => {
    const target = fakeTarget({ initial: { kind: "blur" } });

    const outcome = await runBackgroundEffectReconcile({
      target,
      desired: BACKGROUND_OFF,
      failed: new Set(),
    });

    expect(target.calls.clear).toHaveBeenCalledTimes(1);
    expect(target.calls.attach).not.toHaveBeenCalled();
    expect(outcome).toEqual({ action: "cleared" });
  });

  test("switches in place between two live effects (no re-attach)", async () => {
    const next = { kind: "image", image: { source: "catalog", id: "office" } } as const;
    const target = fakeTarget({ initial: { kind: "blur" } });

    const outcome = await runBackgroundEffectReconcile({ target, desired: next, failed: new Set() });

    expect(target.calls.switch).toHaveBeenCalledTimes(1);
    expect(target.calls.attach).not.toHaveBeenCalled();
    expect(outcome).toEqual({ action: "switched", effect: next });
  });

  test("a failed attach fails open to the raw camera and arms the guard", async () => {
    const target = fakeTarget({ attach: () => Promise.reject(new Error("wasm 404")) });
    const failed = new Set<string>();

    const outcome = await runBackgroundEffectReconcile({ target, desired: { kind: "blur" }, failed });

    // Nothing was attached, so the camera is already raw — do not over-clear.
    expect(target.calls.clear).not.toHaveBeenCalled();
    expect(target.currentEffect()).toEqual(BACKGROUND_OFF);
    expect(failed.has(backgroundEffectKey({ kind: "blur" }))).toBe(true);
    expect(outcome).toMatchObject({ action: "failed", effect: { kind: "blur" } });
  });

  test("a failed switch clears the previously-live effect rather than stranding it", async () => {
    const next = { kind: "image", image: { source: "custom", ref: "u1" } } as const;
    const target = fakeTarget({
      initial: { kind: "blur" },
      switch: () => Promise.reject(new Error("image decode failed")),
    });
    const failed = new Set<string>();

    const outcome = await runBackgroundEffectReconcile({ target, desired: next, failed });

    expect(target.calls.clear).toHaveBeenCalledTimes(1);
    expect(target.currentEffect()).toEqual(BACKGROUND_OFF);
    expect(failed.has(backgroundEffectKey(next))).toBe(true);
    expect(outcome).toMatchObject({ action: "failed", effect: next });
  });
});

import { describe, expect, mock, test } from "bun:test";
import {
  decideAiNoiseFilterAction,
  runAiNoiseFilterReconcile,
  type ProcessorTarget,
} from "../src/lib/calls/ai-noise-filter/reconcile";
import { processorName, type NoiseModelId } from "../src/lib/calls/ai-noise-filter/model-id";

/** A fake processor: opaque to the reconciler, carries a name like the real one. */
type FakeProcessor = { name: string };

/** A fake live mic track whose attach/clear move an in-memory processor name. */
function fakeTarget(initial?: string) {
  let name: string | undefined = initial;
  const attach = mock(async (p: FakeProcessor) => {
    name = p.name;
  });
  const clear = mock(async () => {
    name = undefined;
  });
  const target: ProcessorTarget<FakeProcessor> = {
    currentProcessorName: () => name,
    attach,
    clear,
  };
  return { target, attach, clear };
}

const makeProcessor = (model: NoiseModelId): Promise<FakeProcessor> =>
  Promise.resolve({ name: processorName(model) });

describe("decideAiNoiseFilterAction — pure attach/stop/none decision", () => {
  const noFailures = new Set<NoiseModelId>();

  test("off requested, nothing attached → none", () => {
    expect(decideAiNoiseFilterAction(null, undefined, noFailures)).toEqual({ type: "none" });
  });

  test("off requested, a model attached → stop", () => {
    expect(decideAiNoiseFilterAction(null, processorName("rnnoise"), noFailures)).toEqual({
      type: "stop",
    });
  });

  test("model requested, nothing attached → attach it", () => {
    expect(decideAiNoiseFilterAction("rnnoise", undefined, noFailures)).toEqual({
      type: "attach",
      model: "rnnoise",
    });
  });

  test("same model already attached → none (idempotent, no glitchy re-attach)", () => {
    expect(
      decideAiNoiseFilterAction("rnnoise", processorName("rnnoise"), noFailures),
    ).toEqual({ type: "none" });
  });

  test("a different model attached → attach the newly desired one (switch)", () => {
    expect(decideAiNoiseFilterAction("dtln", processorName("rnnoise"), noFailures)).toEqual({
      type: "attach",
      model: "dtln",
    });
  });

  test("desired model is in the failure guard, bare track → none (no retry loop)", () => {
    const failed = new Set<NoiseModelId>(["rnnoise"]);
    expect(decideAiNoiseFilterAction("rnnoise", undefined, failed)).toEqual({ type: "none" });
  });

  test("desired model guarded but a wrong model still attached → stop it", () => {
    const failed = new Set<NoiseModelId>(["dtln"]);
    expect(decideAiNoiseFilterAction("dtln", processorName("rnnoise"), failed)).toEqual({
      type: "stop",
    });
  });
});

describe("runAiNoiseFilterReconcile — performs the decided action", () => {
  test("attaches the made processor and reports it", async () => {
    const { target, attach } = fakeTarget();
    const failedModels = new Set<NoiseModelId>();
    const outcome = await runAiNoiseFilterReconcile({
      target,
      desired: "rnnoise",
      makeProcessor,
      failedModels,
    });
    expect(attach).toHaveBeenCalledTimes(1);
    expect(outcome).toEqual({ action: "attached", model: "rnnoise" });
  });

  test("clears the processor when off is requested", async () => {
    const { target, clear } = fakeTarget(processorName("rnnoise"));
    const outcome = await runAiNoiseFilterReconcile({
      target,
      desired: null,
      makeProcessor,
      failedModels: new Set(),
    });
    expect(clear).toHaveBeenCalledTimes(1);
    expect(outcome).toEqual({ action: "stopped" });
  });

  test("does nothing when the right model is already attached", async () => {
    const { target, attach, clear } = fakeTarget(processorName("dtln"));
    const outcome = await runAiNoiseFilterReconcile({
      target,
      desired: "dtln",
      makeProcessor,
      failedModels: new Set(),
    });
    expect(attach).not.toHaveBeenCalled();
    expect(clear).not.toHaveBeenCalled();
    expect(outcome).toEqual({ action: "none" });
  });

  test("a failing makeProcessor fails open and arms the per-model guard", async () => {
    const { target, attach } = fakeTarget();
    const failedModels = new Set<NoiseModelId>();
    const boom = () => Promise.reject(new Error("wasm fetch 404"));
    const outcome = await runAiNoiseFilterReconcile({
      target,
      desired: "deepfilternet",
      makeProcessor: boom,
      failedModels,
    });
    expect(attach).not.toHaveBeenCalled();
    expect(failedModels.has("deepfilternet")).toBe(true);
    expect(outcome.action).toBe("failed");
  });

  test("a failing attach also arms the guard", async () => {
    const target: ProcessorTarget<FakeProcessor> = {
      currentProcessorName: () => undefined,
      attach: () => Promise.reject(new Error("AudioWorklet addModule failed")),
      clear: async () => undefined,
    };
    const failedModels = new Set<NoiseModelId>();
    const outcome = await runAiNoiseFilterReconcile({
      target,
      desired: "rnnoise",
      makeProcessor,
      failedModels,
    });
    expect(failedModels.has("rnnoise")).toBe(true);
    expect(outcome.action).toBe("failed");
  });

  test("after a failure, a re-reconcile does not retry (guard holds, no make call)", async () => {
    const { target } = fakeTarget();
    const failedModels = new Set<NoiseModelId>();
    const make = mock((model: NoiseModelId) =>
      model === "rnnoise"
        ? Promise.reject(new Error("fail"))
        : Promise.resolve({ name: processorName(model) }),
    );
    await runAiNoiseFilterReconcile({ target, desired: "rnnoise", makeProcessor: make, failedModels });
    const second = await runAiNoiseFilterReconcile({
      target,
      desired: "rnnoise",
      makeProcessor: make,
      failedModels,
    });
    expect(make).toHaveBeenCalledTimes(1); // not retried the second time
    expect(second).toEqual({ action: "none" });
  });
});

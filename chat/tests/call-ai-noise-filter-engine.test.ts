import { describe, expect, mock, test } from "bun:test";
import { Track } from "livekit-client";
import { CallEngine } from "../src/lib/calls/engine";
import { processorName, type NoiseModelId } from "../src/lib/calls/ai-noise-filter/model-id";
import type { AiNoiseFilterState } from "../src/lib/calls/ai-noise-filter/mic-ai-noise-filter";
import type { AudioNoiseProcessor } from "../src/lib/calls/ai-noise-filter/processor";

/** A fake processor: opaque to the engine, carries the encoded name. */
const fakeProcessor = (model: NoiseModelId): AudioNoiseProcessor =>
  ({ name: processorName(model) }) as unknown as AudioNoiseProcessor;

function engineWithMic(opts: {
  initialProcessor?: AudioNoiseProcessor;
  isMuted?: boolean;
  make?: (model: NoiseModelId) => Promise<AudioNoiseProcessor>;
}) {
  let current = opts.initialProcessor;
  const setProcessor = mock(async (p: AudioNoiseProcessor) => {
    current = p;
  });
  const stopProcessor = mock(async () => {
    current = undefined;
  });
  const track = {
    getProcessor: () => current,
    setProcessor,
    stopProcessor,
  };
  const publication = { isMuted: opts.isMuted ?? false, track };
  const localParticipant = { getTrackPublication: () => publication };
  const make =
    opts.make ?? ((model: NoiseModelId) => Promise.resolve(fakeProcessor(model)));
  const engine = new CallEngine({ makeAiNoiseProcessor: make });
  (engine as unknown as { room: unknown }).room = { localParticipant };

  const states: AiNoiseFilterState[] = [];
  engine.on("aiNoiseFilterChanged", (s) => states.push(s));
  const errors: { model: NoiseModelId; error: unknown }[] = [];
  engine.on("aiNoiseFilterError", (e) => errors.push(e));

  return { engine, setProcessor, stopProcessor, track, states, errors };
}

describe("CallEngine — AI noise filter reconcile", () => {
  test("selecting a model attaches its processor and emits the verified state", async () => {
    const { engine, setProcessor, states } = engineWithMic({});

    await engine.setAiNoiseModel("rnnoise");

    expect(setProcessor).toHaveBeenCalledTimes(1);
    expect(setProcessor.mock.calls[0]?.[0]?.name).toBe(processorName("rnnoise"));
    expect(states.at(-1)).toEqual({ kind: "active", model: "rnnoise" });
  });

  test("selecting off removes the processor and reports model=null", async () => {
    const { engine, stopProcessor, states } = engineWithMic({
      initialProcessor: fakeProcessor("rnnoise"),
    });

    await engine.setAiNoiseModel(null);

    expect(stopProcessor).toHaveBeenCalledTimes(1);
    expect(states.at(-1)).toEqual({ kind: "active", model: null });
  });

  test("re-selecting the already-attached model is idempotent (no re-attach)", async () => {
    const { engine, setProcessor, stopProcessor } = engineWithMic({
      initialProcessor: fakeProcessor("dtln"),
    });

    await engine.setAiNoiseModel("dtln");

    expect(setProcessor).not.toHaveBeenCalled();
    expect(stopProcessor).not.toHaveBeenCalled();
  });

  test("a failed attach fails open (no processor) and emits a typed error", async () => {
    const make = mock((_model: NoiseModelId) =>
      Promise.reject(new Error("wasm fetch 404")),
    );
    const { engine, setProcessor, errors, states } = engineWithMic({ make });

    await engine.setAiNoiseModel("dtln");

    expect(setProcessor).not.toHaveBeenCalled();
    expect(errors).toEqual([{ model: "dtln", error: expect.any(Error) }]);
    expect(states.at(-1)).toEqual({ kind: "active", model: null });
  });

  test("a mic-device change defensively keeps the selected model attached", async () => {
    const { engine, states } = engineWithMic({});
    await engine.setAiNoiseModel("rnnoise");

    (
      engine as unknown as { handleActiveDeviceChanged: (k: MediaDeviceKind) => void }
    ).handleActiveDeviceChanged("audioinput");
    await (engine as unknown as { aiReconcileChain: Promise<void> }).aiReconcileChain;

    expect(states.at(-1)).toEqual({ kind: "active", model: "rnnoise" });
  });

  test("reports no-mic when the mic is muted", async () => {
    const { engine, states } = engineWithMic({ isMuted: true });

    await engine.setAiNoiseModel("rnnoise");

    expect(states.at(-1)).toEqual({ kind: "no-mic" });
  });
});

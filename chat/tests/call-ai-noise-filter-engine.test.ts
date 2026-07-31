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
  const restartTrack = mock(async (_constraints: { noiseSuppression?: boolean }) => undefined);
  const track = {
    getProcessor: () => current,
    setProcessor,
    stopProcessor,
    restartTrack,
    mediaStreamTrack: {
      readyState: "live" as const,
      getSettings: () => ({ deviceId: "mic-1", noiseSuppression: true }),
    },
  };
  const publication = { isMuted: opts.isMuted ?? false, track };
  const localParticipant = { getTrackPublication: () => publication };
  const make =
    opts.make ?? ((model: NoiseModelId) => Promise.resolve(fakeProcessor(model)));
  const engine = new CallEngine({ makeAiNoiseProcessor: make });
  const room = {
    localParticipant,
    off() {
      return room;
    },
    async disconnect() {},
  };
  (engine as unknown as { room: unknown }).room = room;

  const states: AiNoiseFilterState[] = [];
  engine.on("aiNoiseFilterChanged", (s) => states.push(s));
  const errors: { model: NoiseModelId; error: unknown }[] = [];
  engine.on("aiNoiseFilterError", (e) => errors.push(e));

  return { engine, setProcessor, stopProcessor, restartTrack, track, states, errors };
}

/** Did any restartTrack call force browser noise suppression off? */
const forcedNsOff = (restart: ReturnType<typeof mock>): boolean =>
  restart.mock.calls.some((c) => (c[0] as { noiseSuppression?: boolean })?.noiseSuppression === false);

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

  test("a successful attach forces browser noise suppression off", async () => {
    const { engine, restartTrack } = engineWithMic({});

    await engine.setAiNoiseModel("rnnoise");
    await (engine as unknown as { aiReconcileChain: Promise<void> }).aiReconcileChain;

    // The model supersedes browser NS once it is actually running.
    expect(forcedNsOff(restartTrack)).toBe(true);
  });

  test("a FAILED attach never forces browser noise suppression off (no worse than baseline)", async () => {
    const make = mock((_m: NoiseModelId) => Promise.reject(new Error("wasm 404")));
    const { engine, restartTrack, errors } = engineWithMic({ make });

    await engine.setAiNoiseModel("rnnoise");
    await (engine as unknown as { aiReconcileChain: Promise<void> }).aiReconcileChain;

    expect(errors).toHaveLength(1);
    // The verified model is null (nothing attached), so effective constraints
    // keep the user's NS — we must not leave the mic with NS off and no filter.
    expect(forcedNsOff(restartTrack)).toBe(false);
  });

  test("a fresh mic publish re-forces NS off for a still-selected model (no stale flag)", async () => {
    // Unpublish→republish keeps the model selected but starts a NEW capture
    // from stored prefs (NS on). The publish handler must reset the
    // applied-capture flag so the post-attach sync flips NS off again.
    const { engine, restartTrack, track } = engineWithMic({});
    const internals = engine as unknown as {
      desiredAiNoiseModel: string | null;
      appliedModelActiveForCapture: boolean;
      aiReconcileChain: Promise<void>;
      handleLocalTrackPublished: (p: unknown, who: unknown) => void;
    };
    // Simulate the post-unpublish state: model still desired, stale flag true.
    internals.desiredAiNoiseModel = "rnnoise";
    internals.appliedModelActiveForCapture = true;

    internals.handleLocalTrackPublished(
      { track, kind: Track.Kind.Audio, source: Track.Source.Microphone, trackSid: "sid-1" },
      { identity: "me" },
    );
    await internals.aiReconcileChain;

    expect(forcedNsOff(restartTrack)).toBe(true);
  });

  test("emits one consistent combined snapshot per settled change", async () => {
    const { engine } = engineWithMic({});
    const snapshots: { processing: { kind: string }; aiNoiseFilter: { kind: string; model?: unknown } }[] = [];
    engine.on("verifiedMicProcessingChanged", (s) =>
      snapshots.push(s as never),
    );

    await engine.setAiNoiseModel("rnnoise");
    await (engine as unknown as { aiReconcileChain: Promise<void> }).aiReconcileChain;

    const last = snapshots.at(-1);
    // Both layers computed together from the live track: the model is on AND
    // the snapshot is internally consistent (no impossible {NS-on, model-on}).
    expect(last?.aiNoiseFilter).toEqual({ kind: "active", model: "rnnoise" });
    expect(last?.processing.kind).toBe("active");
  });

  test("a delayed processor from call N is destroyed instead of attaching after reconnect", async () => {
    let release: () => void = () => {};
    let started: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const makeStarted = new Promise<void>((resolve) => {
      started = resolve;
    });
    const destroy = mock(async () => undefined);
    const processor = {
      ...fakeProcessor("rnnoise"),
      destroy,
    } as AudioNoiseProcessor;
    const { engine, setProcessor } = engineWithMic({
      make: async () => {
        started();
        await gate;
        return processor;
      },
    });

    const staleReconcile = engine.setAiNoiseModel("rnnoise");
    await makeStarted;
    await engine.disconnect();
    const nextRoom = { localParticipant: { getTrackPublication: () => undefined } };
    const internals = engine as unknown as { room: unknown; connectGeneration: number };
    internals.room = nextRoom;
    internals.connectGeneration += 1;
    release();
    await staleReconcile;

    expect(setProcessor).not.toHaveBeenCalled();
    expect(destroy).toHaveBeenCalledTimes(1);
  });
});

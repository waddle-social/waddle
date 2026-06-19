import { describe, expect, mock, test } from "bun:test";
import { Track } from "livekit-client";
import { CallEngine } from "../src/lib/calls/engine";
import type {
  VirtualBackgroundEffect,
  VideoBackgroundProcessor,
} from "../src/lib/calls/virtual-background/processor";

const blurEffect: VirtualBackgroundEffect = { kind: "blur" };

const fakeProcessor = (effect: VirtualBackgroundEffect): VideoBackgroundProcessor =>
  ({ name: `waddle:virtual-background:${effect.kind}` }) as VideoBackgroundProcessor;

function engineWithCamera(opts: {
  initialProcessor?: VideoBackgroundProcessor;
  isMuted?: boolean;
  make?: (effect: VirtualBackgroundEffect) => Promise<VideoBackgroundProcessor>;
}) {
  let current = opts.initialProcessor;
  const setProcessor = mock(async (p: VideoBackgroundProcessor) => {
    current = p;
  });
  const stopProcessor = mock(async () => {
    current = undefined;
  });
  const track = {
    getProcessor: () => current,
    setProcessor,
    stopProcessor,
    mediaStreamTrack: {
      readyState: "live" as const,
      getSettings: () => ({ deviceId: "camera-1" }),
    },
  };
  const publication = { isMuted: opts.isMuted ?? false, track };
  const localParticipant = { getTrackPublication: () => publication };
  const make = opts.make ?? ((effect: VirtualBackgroundEffect) => Promise.resolve(fakeProcessor(effect)));
  const engine = new CallEngine({ makeVirtualBackgroundProcessor: make });
  (engine as unknown as { room: unknown }).room = { localParticipant };

  const states: VirtualBackgroundEffect[] = [];
  engine.on("virtualBackgroundChanged", (s) => states.push(s));
  const errors: { effect: VirtualBackgroundEffect; error: unknown }[] = [];
  engine.on("virtualBackgroundError", (e) => errors.push(e));

  return { engine, setProcessor, stopProcessor, track, states, errors };
}

describe("CallEngine — virtual background reconcile", () => {
  test("selecting blur attaches its processor to the live camera", async () => {
    const { engine, setProcessor, states } = engineWithCamera({});

    await engine.setVirtualBackground(blurEffect);

    expect(setProcessor).toHaveBeenCalledTimes(1);
    expect(setProcessor.mock.calls[0]?.[0]?.name).toBe("waddle:virtual-background:blur");
    expect(states.at(-1)).toEqual(blurEffect);
  });

  test("selecting an image replacement attaches and verifies that image", async () => {
    const imageEffect: VirtualBackgroundEffect = {
      kind: "image",
      imageUrl: "data:image/png;base64,ZmFrZS1pbWFnZQ==",
    };
    const { engine, setProcessor, states } = engineWithCamera({
      make: (effect) =>
        Promise.resolve({
          name: `waddle:virtual-background:${effect.kind}`,
        } as VideoBackgroundProcessor),
    });

    await engine.setVirtualBackground(imageEffect);

    expect(setProcessor).toHaveBeenCalledTimes(1);
    expect(states.at(-1)).toEqual(imageEffect);
  });

  test("selecting off removes the attached processor", async () => {
    const { engine, stopProcessor, states } = engineWithCamera({
      initialProcessor: fakeProcessor(blurEffect),
    });

    await engine.setVirtualBackground({ kind: "off" });

    expect(stopProcessor).toHaveBeenCalledTimes(1);
    expect(states.at(-1)).toEqual({ kind: "off" });
  });

  test("a failed attach fails open and emits a typed error", async () => {
    const make = mock((_effect: VirtualBackgroundEffect) =>
      Promise.reject(new Error("mediapipe wasm 404")),
    );
    const { engine, setProcessor, stopProcessor, states, errors } = engineWithCamera({ make });

    await engine.setVirtualBackground(blurEffect);

    expect(setProcessor).not.toHaveBeenCalled();
    expect(stopProcessor).toHaveBeenCalledTimes(1);
    expect(errors).toEqual([{ effect: blurEffect, error: expect.any(Error) }]);
    expect(states.at(-1)).toEqual({ kind: "off" });
  });

  test("reports off while no unmuted camera is available", async () => {
    const { engine, setProcessor, states } = engineWithCamera({ isMuted: true });

    await engine.setVirtualBackground(blurEffect);

    expect(setProcessor).not.toHaveBeenCalled();
    expect(states.at(-1)).toEqual({ kind: "off" });
  });

  test("a fresh camera publish clears the failure guard and retries the selected effect", async () => {
    let fail = true;
    const make = mock((effect: VirtualBackgroundEffect) =>
      fail ? Promise.reject(new Error("first load failed")) : Promise.resolve(fakeProcessor(effect)),
    );
    const { engine, setProcessor, track, states, errors } = engineWithCamera({ make });

    await engine.setVirtualBackground(blurEffect);
    fail = false;
    (
      engine as unknown as {
        handleLocalTrackPublished: (p: unknown, who: unknown) => void;
        virtualBackgroundReconcileChain: Promise<void>;
      }
    ).handleLocalTrackPublished(
      { track, kind: Track.Kind.Video, source: Track.Source.Camera, trackSid: "cam-1" },
      { identity: "me" },
    );
    await (
      engine as unknown as { virtualBackgroundReconcileChain: Promise<void> }
    ).virtualBackgroundReconcileChain;

    expect(errors).toHaveLength(1);
    expect(setProcessor).toHaveBeenCalledTimes(1);
    expect(states.at(-1)).toEqual(blurEffect);
  });

  test("a camera device change clears the failure guard and retries the selected effect", async () => {
    let fail = true;
    const make = mock((effect: VirtualBackgroundEffect) =>
      fail ? Promise.reject(new Error("first load failed")) : Promise.resolve(fakeProcessor(effect)),
    );
    const { engine, setProcessor, states } = engineWithCamera({ make });

    await engine.setVirtualBackground(blurEffect);
    fail = false;
    (
      engine as unknown as {
        handleActiveDeviceChanged: (kind: MediaDeviceKind) => void;
        virtualBackgroundReconcileChain: Promise<void>;
      }
    ).handleActiveDeviceChanged("videoinput");
    await (
      engine as unknown as { virtualBackgroundReconcileChain: Promise<void> }
    ).virtualBackgroundReconcileChain;

    expect(setProcessor).toHaveBeenCalledTimes(1);
    expect(states.at(-1)).toEqual(blurEffect);
  });
});

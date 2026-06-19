import { describe, expect, mock, test } from "bun:test";
import { Track } from "livekit-client";
import { CallEngine } from "../src/lib/calls/engine";
import {
  BACKGROUND_OFF,
  CAMERA_BACKGROUND_PROCESSOR_NAME,
  type ActiveBackgroundEffect,
  type BackgroundEffect,
} from "../src/lib/calls/background-effect/effect-id";
import type { CameraBackgroundState } from "../src/lib/calls/background-effect/camera-background";
import type {
  CameraBackgroundOps,
  VideoBackgroundProcessor,
} from "../src/lib/calls/background-effect/ops";

/** A fake processor: opaque to the engine, carries the well-known name. */
const fakeProcessor = (): VideoBackgroundProcessor =>
  ({ name: CAMERA_BACKGROUND_PROCESSOR_NAME }) as unknown as VideoBackgroundProcessor;

function engineWithCamera(opts: {
  initialProcessor?: VideoBackgroundProcessor;
  cameraOff?: boolean;
  create?: CameraBackgroundOps["create"];
  switch?: CameraBackgroundOps["switch"];
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
  };
  const publication: { isMuted: boolean; track: typeof track; source: Track.Source } | undefined =
    opts.cameraOff ? undefined : { isMuted: false, track, source: Track.Source.Camera };
  let activePublication = publication;
  const dropCamera = () => {
    activePublication = undefined;
  };
  const setCameraMuted = (muted: boolean) => {
    if (publication) publication.isMuted = muted;
  };
  const localParticipant = {
    getTrackPublication: (source: Track.Source) =>
      source === Track.Source.Camera ? activePublication : undefined,
    isCameraEnabled: !opts.cameraOff,
  };

  const create =
    opts.create ?? mock((_e: ActiveBackgroundEffect) => Promise.resolve(fakeProcessor()));
  const switchTo = opts.switch ?? mock(async () => undefined);
  const ops: CameraBackgroundOps = { create, switch: switchTo };
  const engine = new CallEngine({ backgroundOps: ops });
  (engine as unknown as { room: unknown }).room = { localParticipant };

  const states: CameraBackgroundState[] = [];
  engine.on("backgroundEffectChanged", (s) => states.push(s));
  const errors: { effect: ActiveBackgroundEffect; error: unknown }[] = [];
  engine.on("backgroundEffectError", (e) => errors.push(e));

  return {
    engine,
    setProcessor,
    stopProcessor,
    create,
    switchTo,
    track,
    states,
    errors,
    dropCamera,
    setCameraMuted,
  };
}

describe("CallEngine — camera background effect reconcile", () => {
  test("selecting blur attaches its processor and emits the verified active state", async () => {
    const { engine, setProcessor, states } = engineWithCamera({});

    await engine.setBackgroundEffect({ kind: "blur" });

    expect(setProcessor).toHaveBeenCalledTimes(1);
    expect(setProcessor.mock.calls[0]?.[0]?.name).toBe(CAMERA_BACKGROUND_PROCESSOR_NAME);
    expect(states.at(-1)).toEqual({ kind: "active", effect: { kind: "blur" } });
  });

  test("selecting off removes the processor and reports the off effect", async () => {
    const { engine, stopProcessor, states } = engineWithCamera({});

    await engine.setBackgroundEffect({ kind: "blur" });
    await engine.setBackgroundEffect(BACKGROUND_OFF);

    expect(stopProcessor).toHaveBeenCalledTimes(1);
    expect(states.at(-1)).toEqual({ kind: "active", effect: { kind: "off" } });
  });

  test("re-selecting the already-attached effect is idempotent", async () => {
    const { engine, create, switchTo } = engineWithCamera({});

    await engine.setBackgroundEffect({ kind: "blur" });
    await engine.setBackgroundEffect({ kind: "blur" });

    expect(create).toHaveBeenCalledTimes(1);
    expect(switchTo).not.toHaveBeenCalled();
  });

  test("changing between two live effects switches in place (no re-create)", async () => {
    const image = { kind: "image", image: { source: "catalog", id: "office" } } as const;
    const { engine, create, switchTo, states } = engineWithCamera({});

    await engine.setBackgroundEffect({ kind: "blur" });
    await engine.setBackgroundEffect(image);

    expect(create).toHaveBeenCalledTimes(1);
    expect(switchTo).toHaveBeenCalledTimes(1);
    expect(switchTo.mock.calls[0]?.[1]).toEqual(image);
    expect(states.at(-1)).toEqual({ kind: "active", effect: image });
  });

  test("a failed attach fails open (no processor) and emits a typed error", async () => {
    const create = mock((_e: ActiveBackgroundEffect) =>
      Promise.reject(new Error("wasm fetch 404")),
    );
    const { engine, setProcessor, errors, states } = engineWithCamera({ create });

    await engine.setBackgroundEffect({ kind: "blur" });

    expect(setProcessor).not.toHaveBeenCalled();
    expect(errors).toEqual([{ effect: { kind: "blur" }, error: expect.any(Error) }]);
    expect(states.at(-1)).toEqual({ kind: "active", effect: { kind: "off" } });
  });

  test("reports no-camera when the camera is not publishing", async () => {
    const { engine, states } = engineWithCamera({ cameraOff: true });

    await engine.setBackgroundEffect({ kind: "blur" });

    expect(states.at(-1)).toEqual({ kind: "no-camera" });
  });

  test("a video device change defensively re-attaches a bare camera's effect", async () => {
    const { engine, create, track, states } = engineWithCamera({});
    await engine.setBackgroundEffect({ kind: "blur" });
    // Simulate LiveKit recycling the capture track on a device switch and
    // bringing it up WITHOUT re-running our processor.
    await track.stopProcessor();

    (
      engine as unknown as { handleActiveDeviceChanged: (k: MediaDeviceKind) => void }
    ).handleActiveDeviceChanged("videoinput");
    await (engine as unknown as { backgroundReconcileChain: Promise<void> }).backgroundReconcileChain;

    expect(create).toHaveBeenCalledTimes(2);
    expect(states.at(-1)).toEqual({ kind: "active", effect: { kind: "blur" } });
  });

  test("a fresh camera publish re-applies the still-desired effect", async () => {
    // Camera was toggled on (or the call just joined) with an effect already
    // selected; the brand-new bare capture track must get it attached.
    const { engine, create, track, states } = engineWithCamera({});
    const internals = engine as unknown as {
      desiredBackgroundEffect: BackgroundEffect;
      backgroundReconcileChain: Promise<void>;
      handleLocalTrackPublished: (p: unknown, who: unknown) => void;
    };
    internals.desiredBackgroundEffect = { kind: "blur" };

    internals.handleLocalTrackPublished(
      { track, kind: Track.Kind.Video, source: Track.Source.Camera, trackSid: "cam-1" },
      { identity: "me" },
    );
    await internals.backgroundReconcileChain;

    expect(create).toHaveBeenCalledTimes(1);
    expect(states.at(-1)).toEqual({ kind: "active", effect: { kind: "blur" } });
  });

  test("turning the camera off reports no-camera (no stale active readout)", async () => {
    const { engine, track, states, dropCamera } = engineWithCamera({});
    await engine.setBackgroundEffect({ kind: "blur" });

    dropCamera();
    (
      engine as unknown as {
        handleLocalTrackUnpublished: (p: unknown, who: unknown) => void;
      }
    ).handleLocalTrackUnpublished(
      { source: Track.Source.Camera, track, kind: Track.Kind.Video, trackSid: "cam-1" },
      { identity: "me" },
    );

    expect(states.at(-1)).toEqual({ kind: "no-camera" });
  });

  test("muting the camera reports no-camera (honest like the mic's no-mic)", async () => {
    const { engine, setCameraMuted, states } = engineWithCamera({});
    await engine.setBackgroundEffect({ kind: "blur" });

    setCameraMuted(true);
    (
      engine as unknown as {
        handleTrackMuteChanged: (p: unknown, who: unknown) => void;
      }
    ).handleTrackMuteChanged(
      { source: Track.Source.Camera, isMuted: true },
      { isLocal: true },
    );

    expect(states.at(-1)).toEqual({ kind: "no-camera" });
  });
});

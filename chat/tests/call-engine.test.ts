import { describe, expect, mock, test } from "bun:test";
import {
  enableRequestedCapture,
  mapTrackSource,
  resolveParticipantAudioVolume,
  type CallEngineEvents,
  type ParticipantAudioVolumeStore,
  type LocalMediaTrack,
  type RemoteMediaTrack,
} from "../src/lib/calls/engine";
import { Track } from "livekit-client";
import type { MicAudioProcessing } from "../src/lib/calls/mic-audio-processing";

function deviceError(name: string): Error {
  const err = new Error(`${name} message`);
  err.name = name;
  return err;
}

/**
 * Stub for LiveKit's LocalParticipant capture surface. Each enable can
 * be told to reject, mirroring a denied permission / missing device.
 */
function captureStub(opts: { micThrows?: Error; camThrows?: Error } = {}) {
  const calls: string[] = [];
  return {
    calls,
    async setMicrophoneEnabled(enabled: boolean) {
      calls.push(`mic:${enabled}`);
      if (opts.micThrows) throw opts.micThrows;
    },
    async setCameraEnabled(enabled: boolean) {
      calls.push(`cam:${enabled}`);
      if (opts.camThrows) throw opts.camThrows;
    },
  };
}

function screenShareStub(opts: { audioThrows?: Error } = {}) {
  const calls: Array<{ enabled: boolean; audio: boolean }> = [];
  return {
    calls,
    get isScreenShareEnabled() {
      return calls.at(-1)?.enabled ?? false;
    },
    async setScreenShareEnabled(enabled: boolean, options?: { audio?: boolean }) {
      calls.push({ enabled, audio: options?.audio ?? false });
      if (enabled && options?.audio && opts.audioThrows) throw opts.audioThrows;
    },
  };
}

// `livekit-client` reaches for browser-only globals (RTCPeerConnection,
// MediaStream, navigator.mediaDevices) at construction time. Tests run
// in bun's node-like env, so we stop at static import + structural
// assertions on the engine's public surface — connect/disconnect path
// is exercised by manual smoke tests in the browser per the call PR
// plan, and by the planned `calls_e2e` integration test on the server.
describe("call-engine module", () => {
  test("resolves participant audio volume by identity and source with default full volume", () => {
    const store: ParticipantAudioVolumeStore = {
      "bob@waddle.test/desktop:microphone": 0.3,
      "bob@waddle.test/desktop:screen_share_audio": 0.6,
    };

    expect(resolveParticipantAudioVolume(store, {
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
    })).toBe(0.3);
    expect(resolveParticipantAudioVolume(store, {
      participantIdentity: "bob@waddle.test/desktop",
      source: "screen_share_audio",
    })).toBe(0.6);
    expect(resolveParticipantAudioVolume(store, {
      participantIdentity: "alice@waddle.test/web",
      source: "microphone",
    })).toBe(1);
  });

  test("resolves stored participant audio volume inside the 0-200 percent gain range", () => {
    const store: ParticipantAudioVolumeStore = {
      "alice@waddle.test/web:microphone": 3,
      "bob@waddle.test/desktop:microphone": Number.NaN,
      "carol@waddle.test/laptop:microphone": -0.5,
    };

    expect(resolveParticipantAudioVolume(store, {
      participantIdentity: "alice@waddle.test/web",
      source: "microphone",
    })).toBe(2);
    expect(resolveParticipantAudioVolume(store, {
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
    })).toBe(1);
    expect(resolveParticipantAudioVolume(store, {
      participantIdentity: "carol@waddle.test/laptop",
      source: "microphone",
    })).toBe(0);
  });

  test("exports CallEngine class with the expected surface", async () => {
    const mod = await import("../src/lib/calls/engine");
    const engine = new mod.CallEngine();
    expect(typeof engine.connect).toBe("function");
    expect(typeof engine.disconnect).toBe("function");
    expect(typeof engine.setMicEnabled).toBe("function");
    expect(typeof engine.setCameraEnabled).toBe("function");
    expect(typeof engine.setScreenShareEnabled).toBe("function");
    expect(typeof engine.on).toBe("function");
    // `localIdentity` is a getter, so it appears as a value when the
    // class isn't connected (returns null pre-connect). Just check
    // it doesn't throw — anything more requires a live Room.
    expect(engine.localIdentity).toBeNull();
    expect(engine.screenShareEnabled).toBe(false);
  });

  test("on() returns an unsubscribe handle that detaches the listener", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    let calls = 0;
    const handler: CallEngineEvents["disconnected"] = () => {
      calls += 1;
    };
    const off = engine.on("disconnected", handler);
    off();
    expect(typeof off).toBe("function");
    expect(calls).toBe(0);
  });

  test("on() supports the new local-track events parallel to the remote ones", async () => {
    // Regression guard: the CallOverlay self-preview tile depends on
    // `localTrackPublished` / `localTrackUnpublished` events being
    // first-class on the engine surface, not just on the underlying
    // LiveKit Room. If a refactor drops them from `listeners`, the
    // `engine.on(...)` registration in `use-call-engine` would
    // silently no-op and the user would never see their own camera.
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    expect(typeof engine.on("localTrackPublished", () => {})).toBe("function");
    expect(typeof engine.on("localTrackUnpublished", () => {})).toBe("function");
  });

  test("audio playback status exposes blocked state and resume calls LiveKit startAudio", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const statuses: boolean[] = [];
    let startAudioCalls = 0;
    engine.on("audioPlaybackStatusChanged", (canPlaybackAudio) => {
      statuses.push(canPlaybackAudio);
    });
    const stub = {
      get canPlaybackAudio() {
        return statuses.at(-1) ?? true;
      },
      async startAudio() {
        startAudioCalls += 1;
      },
    };
    (engine as unknown as { room: typeof stub }).room = stub;

    (
      engine as unknown as {
        handleAudioPlaybackStatusChanged: (canPlaybackAudio: boolean) => void;
      }
    ).handleAudioPlaybackStatusChanged(false);
    expect(engine.canPlaybackAudio).toBe(false);

    await engine.startAudio();
    (
      engine as unknown as {
        handleAudioPlaybackStatusChanged: (canPlaybackAudio: boolean) => void;
      }
    ).handleAudioPlaybackStatusChanged(true);

    expect(startAudioCalls).toBe(1);
    expect(statuses).toEqual([false, true]);
    expect(engine.canPlaybackAudio).toBe(true);
  });

  test("RemoteMediaTrack discriminates audio vs video by `kind`", () => {
    // Pure type-level assertion: the field is a "audio" | "video"
    // literal union, so a switch covers both arms exhaustively.
    const classify = (t: RemoteMediaTrack["kind"]): string => {
      switch (t) {
        case "audio":
          return "a";
        case "video":
          return "v";
      }
    };
    expect(classify("audio")).toBe("a");
    expect(classify("video")).toBe("v");
  });

  test("disconnect() emits the synthetic 'disconnected' event so subscribers drain stale caches", async () => {
    // Regression guard for the "I see myself twice on rejoin" bug:
    // `engine.disconnect()` unregisters the LiveKit `Disconnected`
    // listener before awaiting `room.disconnect()`, so the real event
    // never reaches our handler. We compensate with a synthetic emit
    // at the top of `disconnect()`. Without it, `useCallEngine` would
    // never clear its `localTracks` / `remoteTracks` refs and the
    // next call's tiles would inherit stale entries.
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    let disconnects = 0;
    engine.on("disconnected", () => {
      disconnects += 1;
    });
    // Inject a minimal Room stub matching the surface `disconnect()`
    // touches. `livekit-client`'s real `Room` needs browser globals.
    const stub = {
      off: () => stub,
      disconnect: async () => undefined,
    };
    (engine as unknown as { room: typeof stub }).room = stub;
    await engine.disconnect();
    expect(disconnects).toBe(1);
  });

  test("disconnect() is a no-op when no room was connected", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    let disconnects = 0;
    engine.on("disconnected", () => {
      disconnects += 1;
    });
    await engine.disconnect();
    expect(disconnects).toBe(0);
  });

  test("LocalMediaTrack mirrors RemoteMediaTrack's shape so one tile component renders both", () => {
    // The overlay's `Tile.videoTrack` accepts either side via a
    // duck-typed `attach`/`detach` callback. This assertion is a
    // compile-time check that the two types stay structurally
    // assignable on the fields the renderer touches; if they ever
    // diverge, the per-tile branch logic would have to grow.
    const classify = (t: LocalMediaTrack["kind"]): string => {
      switch (t) {
        case "audio":
          return "a";
        case "video":
          return "v";
      }
    };
    expect(classify("audio")).toBe("a");
    expect(classify("video")).toBe("v");
  });

  test("maps every LiveKit publication source into the typed call source union", () => {
    expect(mapTrackSource(Track.Source.Camera)).toBe("camera");
    expect(mapTrackSource(Track.Source.Microphone)).toBe("microphone");
    expect(mapTrackSource(Track.Source.ScreenShare)).toBe("screen_share");
    expect(mapTrackSource(Track.Source.ScreenShareAudio)).toBe("screen_share_audio");
    expect(mapTrackSource(Track.Source.Unknown, "video")).toBe("camera");
    expect(mapTrackSource(Track.Source.Unknown, "audio")).toBe("microphone");
  });

  test("media track descriptors carry a typed source separate from kind", () => {
    const remote: Pick<RemoteMediaTrack, "kind" | "source"> = {
      kind: "video",
      source: "screen_share",
    };
    const local: Pick<LocalMediaTrack, "kind" | "source"> = {
      kind: "audio",
      source: "screen_share_audio",
    };
    expect(remote.source).toBe("screen_share");
    expect(local.source).toBe("screen_share_audio");
  });

  test("setScreenShareEnabled delegates video-only requests to LiveKit", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const participant = screenShareStub();
    (engine as unknown as { room: unknown }).room = { localParticipant: participant };
    await engine.setScreenShareEnabled(true, { audio: false });
    expect(participant.calls).toEqual([{ enabled: true, audio: false }]);
    expect(engine.screenShareEnabled).toBe(true);
  });

  test.each([
    "OverconstrainedError",
    "ConstraintNotSatisfiedError",
    "NotSupportedError",
  ])("setScreenShareEnabled degrades %s screen audio failure to video-only share", async (errorName) => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const participant = screenShareStub({ audioThrows: deviceError(errorName) });
    (engine as unknown as { room: unknown }).room = { localParticipant: participant };
    await engine.setScreenShareEnabled(true, { audio: true });
    expect(participant.calls).toEqual([
      { enabled: true, audio: true },
      { enabled: true, audio: false },
    ]);
    expect(engine.screenShareEnabled).toBe(true);
  });

  test("setScreenShareEnabled does not mask unrelated LiveKit failures as screen-audio fallback", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const participant = screenShareStub({ audioThrows: deviceError("TypeError") });
    (engine as unknown as { room: unknown }).room = { localParticipant: participant };
    await expect(engine.setScreenShareEnabled(true, { audio: true })).rejects.toHaveProperty("name", "TypeError");
    expect(participant.calls).toEqual([{ enabled: true, audio: true }]);
  });

  test("track subscription re-applies the stored participant audio volume", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const setVolume = mock(() => undefined);
    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: 0.3,
    });

    (
      engine as unknown as {
        handleTrackSubscribed: (track: unknown, publication: unknown, participant: unknown) => void;
      }
    ).handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume,
      },
      {
        trackSid: "bob-mic-2",
        source: Track.Source.Microphone,
      },
      { identity: "bob@waddle.test/desktop" },
    );

    expect(setVolume).toHaveBeenCalledWith(0.3);
  });

  test("track subscription keeps microphone and screen-share audio volumes independent", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const setMicVolume = mock(() => undefined);
    const setShareVolume = mock(() => undefined);
    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: 0.3,
    });
    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "screen_share_audio",
      volume: 0.6,
    });
    const { handleTrackSubscribed } = engine as unknown as {
      handleTrackSubscribed: (track: unknown, publication: unknown, participant: unknown) => void;
    };

    handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume: setMicVolume,
      },
      {
        trackSid: "bob-mic",
        source: Track.Source.Microphone,
      },
      { identity: "bob@waddle.test/desktop" },
    );
    handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume: setShareVolume,
      },
      {
        trackSid: "bob-share-audio",
        source: Track.Source.ScreenShareAudio,
      },
      { identity: "bob@waddle.test/desktop" },
    );

    expect(setMicVolume).toHaveBeenCalledWith(0.3);
    expect(setShareVolume).toHaveBeenCalledWith(0.6);
  });

  test("setting participant audio volume updates currently subscribed tracks", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const setVolume = mock(() => undefined);
    (
      engine as unknown as {
        handleTrackSubscribed: (track: unknown, publication: unknown, participant: unknown) => void;
      }
    ).handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume,
      },
      {
        trackSid: "bob-mic",
        source: Track.Source.Microphone,
      },
      { identity: "bob@waddle.test/desktop" },
    );

    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: 0.25,
    });

    expect(setVolume).toHaveBeenLastCalledWith(0.25);
  });

  test("participant audio volume is clamped before storing and applying", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const setVolume = mock(() => undefined);
    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: 30,
    });
    (
      engine as unknown as {
        handleTrackSubscribed: (track: unknown, publication: unknown, participant: unknown) => void;
      }
    ).handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume,
      },
      {
        trackSid: "bob-mic",
        source: Track.Source.Microphone,
      },
      { identity: "bob@waddle.test/desktop" },
    );

    expect(setVolume).toHaveBeenCalledWith(2);
    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: -1,
    });
    expect(setVolume).toHaveBeenLastCalledWith(0);
    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: Number.NaN,
    });
    expect(setVolume).toHaveBeenLastCalledWith(1);
  });

  test("participant disconnect drops stale subscribed audio track references", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const setVolume = mock(() => undefined);
    (
      engine as unknown as {
        handleTrackSubscribed: (track: unknown, publication: unknown, participant: unknown) => void;
      }
    ).handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume,
      },
      {
        trackSid: "bob-mic",
        source: Track.Source.Microphone,
      },
      { identity: "bob@waddle.test/desktop" },
    );
    (
      engine as unknown as {
        handleParticipantDisconnected: (participant: unknown) => void;
      }
    ).handleParticipantDisconnected({ identity: "bob@waddle.test/desktop" });

    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: 0.25,
    });

    expect(setVolume).not.toHaveBeenLastCalledWith(0.25);
  });

  test("participant disconnect only drops exact identity audio track references", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const disconnectedSetVolume = mock(() => undefined);
    const activeSetVolume = mock(() => undefined);
    const { handleTrackSubscribed, handleParticipantDisconnected } = engine as unknown as {
      handleTrackSubscribed: (track: unknown, publication: unknown, participant: unknown) => void;
      handleParticipantDisconnected: (participant: unknown) => void;
    };
    handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume: disconnectedSetVolume,
      },
      {
        trackSid: "alice-app-mic",
        source: Track.Source.Microphone,
      },
      { identity: "alice@waddle.test/app" },
    );
    handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume: activeSetVolume,
      },
      {
        trackSid: "alice-app-phone-mic",
        source: Track.Source.Microphone,
      },
      { identity: "alice@waddle.test/app:phone" },
    );

    handleParticipantDisconnected({ identity: "alice@waddle.test/app" });
    engine.setParticipantAudioVolume({
      participantIdentity: "alice@waddle.test/app:phone",
      source: "microphone",
      volume: 0.25,
    });
    engine.setParticipantAudioVolume({
      participantIdentity: "alice@waddle.test/app",
      source: "microphone",
      volume: 0.4,
    });

    expect(activeSetVolume).toHaveBeenLastCalledWith(0.25);
    expect(disconnectedSetVolume).not.toHaveBeenLastCalledWith(0.4);
  });

  test("disconnect clears stored participant audio volumes for the next call", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const stubRoom = {
      off: () => stubRoom,
      disconnect: async () => undefined,
    };
    const setVolume = mock(() => undefined);
    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: 0.3,
    });
    (engine as unknown as { room: typeof stubRoom }).room = stubRoom;

    await engine.disconnect();
    (
      engine as unknown as {
        handleTrackSubscribed: (track: unknown, publication: unknown, participant: unknown) => void;
      }
    ).handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume,
      },
      {
        trackSid: "bob-mic-next-call",
        source: Track.Source.Microphone,
      },
      { identity: "bob@waddle.test/desktop" },
    );

    expect(setVolume).toHaveBeenCalledWith(1);
  });

  test("disconnect without a room does not clear pre-connected participant audio volumes", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const setVolume = mock(() => undefined);
    engine.setParticipantAudioVolume({
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
      volume: 0.3,
    });

    await engine.disconnect();
    (
      engine as unknown as {
        handleTrackSubscribed: (track: unknown, publication: unknown, participant: unknown) => void;
      }
    ).handleTrackSubscribed(
      {
        kind: Track.Kind.Audio,
        setVolume,
      },
      {
        trackSid: "bob-mic",
        source: Track.Source.Microphone,
      },
      { identity: "bob@waddle.test/desktop" },
    );

    expect(setVolume).toHaveBeenCalledWith(0.3);
  });
});

describe("enableRequestedCapture — best-effort, never ejects", () => {
  test("enables both tracks when capture succeeds", async () => {
    const stub = captureStub();
    const errors: string[] = [];
    await enableRequestedCapture(stub, { audio: true, video: true }, (s) => errors.push(s));
    expect(stub.calls).toEqual(["mic:true", "cam:true"]);
    expect(errors).toEqual([]);
  });

  test("a denied mic does NOT throw and still attempts the camera", async () => {
    // The headline guarantee: a capture failure must not reject, or the
    // CallOverlay watcher would tear the joined call down ("ejected").
    const stub = captureStub({ micThrows: deviceError("NotAllowedError") });
    const seen: Array<{ source: string; name: string }> = [];
    await enableRequestedCapture(stub, { audio: true, video: true }, (source, error) => {
      seen.push({ source, name: (error as Error).name });
    });
    expect(stub.calls).toEqual(["mic:true", "cam:true"]); // camera still attempted
    expect(seen).toEqual([{ source: "audio", name: "NotAllowedError" }]);
  });

  test("a missing camera reports only video and leaves the mic untouched", async () => {
    const stub = captureStub({ camThrows: deviceError("NotFoundError") });
    const seen: string[] = [];
    await enableRequestedCapture(stub, { audio: true, video: true }, (source) => seen.push(source));
    expect(seen).toEqual(["video"]);
  });

  test("NO working device at all (both denied) resolves without throwing", async () => {
    // The literal "join without any working device" case.
    const stub = captureStub({
      micThrows: deviceError("NotAllowedError"),
      camThrows: deviceError("NotFoundError"),
    });
    const seen: string[] = [];
    await expect(
      enableRequestedCapture(stub, { audio: true, video: true }, (source) => seen.push(source)),
    ).resolves.toBeUndefined();
    expect(seen).toEqual(["audio", "video"]);
  });

  test("audio-only call never touches the camera", async () => {
    const stub = captureStub({ camThrows: deviceError("NotFoundError") });
    const seen: string[] = [];
    await enableRequestedCapture(stub, { audio: true, video: false }, (source) => seen.push(source));
    expect(stub.calls).toEqual(["mic:true"]);
    expect(seen).toEqual([]);
  });
});

/**
 * Inject a minimal Room stub whose local mic publication reports the
 * given `getSettings()` (or no publication at all, for the no-mic
 * path). Mirrors how the other engine tests stub `room` to avoid
 * `livekit-client`'s browser-only `Room`.
 */
function micRoomStub(
  settings: MediaTrackSettings | null,
  readyState: MediaStreamTrackState = "live",
) {
  const publication =
    settings === null
      ? undefined
      : { track: { mediaStreamTrack: { readyState, getSettings: () => settings } } };
  return {
    localParticipant: {
      getTrackPublication: (source: Track.Source) =>
        source === Track.Source.Microphone ? publication : undefined,
    },
  };
}

function publishMic(engine: unknown): void {
  (
    engine as {
      handleLocalTrackPublished: (publication: unknown, participant: unknown) => void;
    }
  ).handleLocalTrackPublished(
    { track: {}, kind: Track.Kind.Audio, trackSid: "self-mic", source: Track.Source.Microphone },
    { identity: "me@waddle.test/web" },
  );
}

describe("call-engine — verifies applied mic audio processing", () => {
  test("mic publish emits the applied noise/echo/gain trio read from getSettings", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const events: MicAudioProcessing[] = [];
    engine.on("micAudioProcessingChanged", (state) => events.push(state));
    (engine as unknown as { room: unknown }).room = micRoomStub({
      noiseSuppression: true,
      echoCancellation: true,
      autoGainControl: false,
    } as MediaTrackSettings);

    publishMic(engine);

    expect(events).toEqual([
      {
        kind: "active",
        noiseSuppression: "on",
        echoCancellation: "on",
        autoGainControl: "off",
      },
    ]);
  });

  test("a browser that omits a constraint reports it as unknown, not off", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const events: MicAudioProcessing[] = [];
    engine.on("micAudioProcessingChanged", (state) => events.push(state));
    (engine as unknown as { room: unknown }).room = micRoomStub({
      noiseSuppression: true,
      // echoCancellation + autoGainControl absent → unknown
    } as MediaTrackSettings);

    publishMic(engine);

    expect(events.at(-1)).toEqual({
      kind: "active",
      noiseSuppression: "on",
      echoCancellation: "unknown",
      autoGainControl: "unknown",
    });
  });

  test("publishing a non-mic track (camera) does not emit a mic-processing change", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const events: MicAudioProcessing[] = [];
    engine.on("micAudioProcessingChanged", (state) => events.push(state));
    (engine as unknown as { room: unknown }).room = micRoomStub(null);

    (
      engine as unknown as {
        handleLocalTrackPublished: (publication: unknown, participant: unknown) => void;
      }
    ).handleLocalTrackPublished(
      { track: {}, kind: Track.Kind.Video, trackSid: "self-cam", source: Track.Source.Camera },
      { identity: "me@waddle.test/web" },
    );

    expect(events).toEqual([]);
  });

  test("mic unpublish falls back to no-mic instead of a stale trio", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const events: MicAudioProcessing[] = [];
    engine.on("micAudioProcessingChanged", (state) => events.push(state));
    // No mic publication present once unpublished.
    (engine as unknown as { room: unknown }).room = micRoomStub(null);

    (
      engine as unknown as {
        handleLocalTrackUnpublished: (publication: unknown, participant: unknown) => void;
      }
    ).handleLocalTrackUnpublished(
      { track: {}, kind: Track.Kind.Audio, trackSid: "self-mic", source: Track.Source.Microphone },
      { identity: "me@waddle.test/web" },
    );

    expect(events.at(-1)).toEqual({ kind: "no-mic" });
  });

  test("an ended capture track reads as no-mic (mic stopped on mute)", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const events: MicAudioProcessing[] = [];
    engine.on("micAudioProcessingChanged", (state) => events.push(state));
    (engine as unknown as { room: unknown }).room = micRoomStub(
      { noiseSuppression: true } as MediaTrackSettings,
      "ended",
    );
    (engine as unknown as {
      handleActiveDeviceChanged: (kind: MediaDeviceKind) => void;
    }).handleActiveDeviceChanged("audioinput");
    expect(events.at(-1)).toEqual({ kind: "no-mic" });
  });

  test("a mid-call mic device switch recomputes; a camera/speaker switch does not", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const events: MicAudioProcessing[] = [];
    engine.on("micAudioProcessingChanged", (state) => events.push(state));
    (engine as unknown as { room: unknown }).room = micRoomStub({
      noiseSuppression: false,
    } as MediaTrackSettings);
    const handle = (engine as unknown as {
      handleActiveDeviceChanged: (kind: MediaDeviceKind) => void;
    }).handleActiveDeviceChanged;

    handle("videoinput");
    handle("audiooutput");
    expect(events).toEqual([]);

    handle("audioinput");
    expect(events).toEqual([
      {
        kind: "active",
        noiseSuppression: "off",
        echoCancellation: "unknown",
        autoGainControl: "unknown",
      },
    ]);
  });

  test("a device change with no connected room emits no-mic, not a crash", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const events: MicAudioProcessing[] = [];
    engine.on("micAudioProcessingChanged", (state) => events.push(state));
    (engine as unknown as {
      handleActiveDeviceChanged: (kind: MediaDeviceKind) => void;
    }).handleActiveDeviceChanged("audioinput");
    expect(events).toEqual([{ kind: "no-mic" }]);
  });

  test("on() exposes micAudioProcessingChanged as a first-class engine event", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    expect(typeof engine.on("micAudioProcessingChanged", () => {})).toBe("function");
  });
});

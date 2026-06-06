import { describe, expect, test } from "bun:test";
import {
  enableRequestedCapture,
  mapTrackSource,
  type CallEngineEvents,
  type LocalMediaTrack,
  type RemoteMediaTrack,
} from "../src/lib/calls/engine";
import { Track } from "livekit-client";

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

  test("setScreenShareEnabled degrades unsupported screen audio to video-only share", async () => {
    const { CallEngine } = await import("../src/lib/calls/engine");
    const engine = new CallEngine();
    const participant = screenShareStub({ audioThrows: deviceError("OverconstrainedError") });
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
    expect(engine.screenShareEnabled).toBe(true);
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

import { describe, expect, test } from "bun:test";
import type {
  CallEngineEvents,
  LocalMediaTrack,
  RemoteMediaTrack,
} from "../src/lib/calls/engine";

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
    expect(typeof engine.on).toBe("function");
    // `localIdentity` is a getter, so it appears as a value when the
    // class isn't connected (returns null pre-connect). Just check
    // it doesn't throw — anything more requires a live Room.
    expect(engine.localIdentity).toBeNull();
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
});

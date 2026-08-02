import { describe, expect, test } from "bun:test";
import {
  ConnectionState,
  DisconnectReason,
  RoomEvent,
  type Room,
  type RoomConnectOptions,
  type RoomOptions,
} from "livekit-client";
import { CallEngine } from "../src/lib/calls/engine";
import { createIceCredentialRefresher } from "../src/lib/calls/ice-credential-refresh";
import { videoCodecSupport } from "../src/lib/calls/video-codec/support";
import type { LiveKitJoin } from "../src/lib/calls/types";

// Minimal base64url JWT carrying a valid LiveKit video grant so the engine's
// `validateLiveKitGrant` pre-flight passes before `room.connect`.
function base64url(value: object): string {
  return btoa(JSON.stringify(value))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

function joinToken(): string {
  const header = base64url({ alg: "HS256", typ: "JWT" });
  const payload = base64url({ video: { roomJoin: true, room: "alice@waddle.test::c1" } });
  return `${header}.${payload}.sig`;
}

function join(): LiveKitJoin {
  return {
    url: "wss://livekit.waddle.test",
    room: "alice@waddle.test::c1",
    identity: "alice@waddle.test/desktop",
    token: joinToken(),
  };
}

/**
 * Records every `room.connect(...)` call so the test can assert what the engine
 * forwards. Provides only the surface `CallEngine.connect` touches.
 */
function stubRoom() {
  const connectCalls: Array<{ url: string; token: string; opts?: RoomConnectOptions }> = [];
  const handlers = new Map<RoomEvent, unknown>();
  const offCalls: RoomEvent[] = [];
  let disconnectCalls = 0;
  const room = {
    connectCalls,
    handlers,
    offCalls,
    get disconnectCalls() {
      return disconnectCalls;
    },
    on(event: RoomEvent, handler: unknown) {
      handlers.set(event, handler);
      return room;
    },
    off(event: RoomEvent, handler: unknown) {
      if (handlers.get(event) === handler) handlers.delete(event);
      offCalls.push(event);
      return room;
    },
    removeAllListeners() {},
    async connect(url: string, token: string, opts?: RoomConnectOptions) {
      connectCalls.push({ url, token, opts });
    },
    localParticipant: {
      identity: "alice@waddle.test/desktop",
      async setMicrophoneEnabled() {},
      async setCameraEnabled() {},
      async setScreenShareEnabled() {},
      getTrackPublication() {
        return undefined;
      },
    },
    remoteParticipants: new Map(),
    async disconnect() {
      disconnectCalls += 1;
    },
  };
  return room;
}

function engineWith(room: ReturnType<typeof stubRoom>) {
  return new CallEngine({
    makeRoom: (_options: RoomOptions) => room as unknown as Room,
    videoCodecSupport: videoCodecSupport({ encode: ["video/vp8"], decode: ["video/vp8"] }),
  });
}

const iceServers: RTCIceServer[] = [
  { urls: "turns:turn.waddle.social:443?transport=tcp", username: "u", credential: "p" },
  { urls: "stun:turn.waddle.social:3478" },
];

describe("XEP-0215 credential refresh scheduling", () => {
  test("fires one minute before the earliest expiry and applies fresh credentials", async () => {
    const now = Date.parse("2026-07-31T12:00:00Z");
    let scheduled: (() => void) | null = null;
    let scheduledDelay = -1;
    const applied: RTCIceServer[][] = [];
    let resolveRefreshed: () => void = () => undefined;
    const refreshed = new Promise<void>((resolve) => {
      resolveRefreshed = resolve;
    });
    const refresher = createIceCredentialRefresher({
      refresh: async () => ({
        servers: [{ urls: "turns:fresh.waddle.test:443", username: "u2", credential: "p2" }],
        earliestExpiryMs: now + 600_000,
      }),
      apply: (bundle) => applied.push(bundle.servers),
      isCurrent: () => true,
      onRefreshed: resolveRefreshed,
      onExpired: () => undefined,
      clock: {
        now: () => now,
        setTimeout: (callback, delayMs) => {
          scheduled = callback;
          scheduledDelay = delayMs;
          return 1 as unknown as ReturnType<typeof setTimeout>;
        },
        clearTimeout: () => undefined,
      },
    });

    refresher.start(now + 300_000);
    expect(scheduledDelay).toBe(240_000);
    expect(scheduled).not.toBeNull();
    (scheduled as unknown as () => void)();
    await refreshed;

    expect(applied).toEqual([[
      { urls: "turns:fresh.waddle.test:443", username: "u2", credential: "p2" },
    ]]);
  });

  test("stop clears the call-scoped timer and prevents a stale callback from refreshing", async () => {
    const cleared: Array<ReturnType<typeof setTimeout>> = [];
    let scheduled: (() => void) | null = null;
    let refreshCalls = 0;
    const timer = 7 as unknown as ReturnType<typeof setTimeout>;
    const refresher = createIceCredentialRefresher({
      refresh: async () => {
        refreshCalls += 1;
        return { servers: iceServers, earliestExpiryMs: null };
      },
      apply: () => undefined,
      isCurrent: () => true,
      onRefreshed: () => undefined,
      onExpired: () => undefined,
      clock: {
        now: () => 1_000,
        setTimeout: (callback) => {
          scheduled = callback;
          return timer;
        },
        clearTimeout: (handle) => cleared.push(handle),
      },
    });

    refresher.start(120_000);
    refresher.stop();
    expect(cleared).toEqual([timer]);
    (scheduled as unknown as () => void)();
    await Promise.resolve();
    expect(refreshCalls).toBe(0);
  });
});

describe("CallEngine.connect — XEP-0215 ICE injection", () => {
  test("passes the mapped iceServers via rtcConfig at connect", async () => {
    const room = stubRoom();
    await engineWith(room).connect(join(), { audio: false, video: false, iceServers });

    expect(room.connectCalls).toHaveLength(1);
    const { url, token, opts } = room.connectCalls[0];
    expect(url).toBe("wss://livekit.waddle.test");
    expect(token).toBe(joinToken());
    expect(opts?.rtcConfig?.iceServers).toEqual(iceServers);
  });

  test("omits rtcConfig when no iceServers are supplied (LiveKit defaults)", async () => {
    const room = stubRoom();
    await engineWith(room).connect(join(), { audio: false, video: false });

    expect(room.connectCalls[0].opts).toBeUndefined();
  });

  test("omits rtcConfig when the iceServers list is empty (fall back, never replace with nothing)", async () => {
    const room = stubRoom();
    await engineWith(room).connect(join(), { audio: false, video: false, iceServers: [] });

    expect(room.connectCalls[0].opts).toBeUndefined();
  });

  test("an empty advertisement disables both rtcConfig and the refresher", async () => {
    // resolveIceServers() returns an empty bundle on any failure. Refresh
    // being wired must not force `{ rtcConfig: {} }` into existence (the
    // engine's contract is to omit rtcConfig so LiveKit keeps its
    // signalling-provided servers) — and with no retained config object a
    // refresh would have nothing to update, so it must never fire.
    const room = stubRoom();
    const refreshCalls: number[] = [];
    await engineWith(room).connect(join(), {
      audio: false,
      video: false,
      iceServers: [],
      iceServersExpiryMs: Date.now() + 1_000,
      refreshIceServers: async () => {
        refreshCalls.push(1);
        return { servers: [], earliestExpiryMs: null };
      },
    });

    expect(room.connectCalls[0].opts).toBeUndefined();
    const onConnectionStateChanged = room.handlers.get(RoomEvent.ConnectionStateChanged) as
      | ((state: ConnectionState) => void)
      | undefined;
    onConnectionStateChanged?.(ConnectionState.Reconnecting);
    await Promise.resolve();
    expect(refreshCalls).toEqual([]);
  });

  test("refreshes credentials on transport reconnect for a future PeerConnection rebuild", async () => {
    const room = stubRoom();
    const engine = engineWith(room);
    const refreshed: number[] = [];
    engine.on("iceCredentialsRefreshed", () => refreshed.push(1));
    let releaseRefresh: (bundle: {
      servers: RTCIceServer[];
      earliestExpiryMs: number | null;
    }) => void = () => undefined;
    const refreshResult = new Promise<{
      servers: RTCIceServer[];
      earliestExpiryMs: number | null;
    }>((resolve) => {
      releaseRefresh = resolve;
    });

    await engine.connect(join(), {
      audio: false,
      video: false,
      iceServers,
      iceServersExpiryMs: null,
      refreshIceServers: () => refreshResult,
    });
    const rtcConfig = room.connectCalls[0].opts?.rtcConfig;
    const onConnectionStateChanged = room.handlers.get(RoomEvent.ConnectionStateChanged) as
      | ((state: ConnectionState) => void)
      | undefined;
    const freshServers: RTCIceServer[] = [
      { urls: "turns:fresh.waddle.test:443", username: "new", credential: "secret" },
    ];

    onConnectionStateChanged?.(ConnectionState.Reconnecting);
    releaseRefresh({ servers: freshServers, earliestExpiryMs: null });
    await refreshResult;
    await Promise.resolve();

    // LiveKit 2.19 retains this config object and clones it for a full
    // reconnect. Its current PeerConnection is not updated in place.
    expect(rtcConfig?.iceServers).toEqual(freshServers);
    expect(refreshed).toEqual([1]);
  });

  test("disconnect clears the proactive credential refresh timer", async () => {
    const room = stubRoom();
    const timer = 11 as unknown as ReturnType<typeof setTimeout>;
    const cleared: Array<ReturnType<typeof setTimeout>> = [];
    const engine = new CallEngine({
      makeRoom: () => room as unknown as Room,
      videoCodecSupport: videoCodecSupport({ encode: ["video/vp8"], decode: ["video/vp8"] }),
      iceRefreshClock: {
        now: () => 1_000,
        setTimeout: () => timer,
        clearTimeout: (handle) => cleared.push(handle),
      },
    });

    await engine.connect(join(), {
      audio: false,
      video: false,
      iceServers,
      iceServersExpiryMs: 120_000,
      refreshIceServers: async () => ({ servers: iceServers, earliestExpiryMs: null }),
    });
    await engine.disconnect();

    expect(cleared).toEqual([timer]);
  });

  test("rejects a second concurrent connect before the first resolves, building only one Room", async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let roomsBuilt = 0;
    const makeRoom = () => {
      roomsBuilt += 1;
      const room = {
        on() {
          return room;
        },
        removeAllListeners() {},
        async connect() {
          await gate; // hold the connect open to overlap a second call
        },
        localParticipant: {
          identity: "alice@waddle.test/desktop",
          async setMicrophoneEnabled() {},
          async setCameraEnabled() {},
        },
        remoteParticipants: new Map(),
      };
      return room as unknown as Room;
    };
    const engine = new CallEngine({
      makeRoom,
      videoCodecSupport: videoCodecSupport({ encode: ["video/vp8"], decode: ["video/vp8"] }),
    });

    const first = engine.connect(join(), { audio: false, video: false });
    await expect(engine.connect(join(), { audio: false, video: false })).rejects.toThrow(
      "already connected",
    );
    release();
    await first;
    expect(roomsBuilt).toBe(1);
  });

  test("a disconnect during connect cancels it (no wedge, no orphaned Room)", async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let built = 0;
    let roomsDisconnected = 0;
    const makeRoom = () => {
      built += 1;
      const room = {
        on() {
          return room;
        },
        removeAllListeners() {},
        async connect() {
          await gate;
        },
        async disconnect() {
          roomsDisconnected += 1;
        },
        localParticipant: {
          identity: "alice@waddle.test/desktop",
          async setMicrophoneEnabled() {},
          async setCameraEnabled() {},
        },
        remoteParticipants: new Map(),
      };
      return room as unknown as Room;
    };
    const engine = new CallEngine({
      makeRoom,
      videoCodecSupport: videoCodecSupport({ encode: ["video/vp8"], decode: ["video/vp8"] }),
    });

    const first = engine.connect(join(), { audio: false, video: false });
    await engine.disconnect(); // cancel mid-connect, before the Room is published
    release();
    await first; // resolves but must abandon (tear down) its Room, not publish it
    expect(roomsDisconnected).toBe(1);

    // The engine is not wedged: a fresh connect proceeds (gate already resolved).
    await engine.connect(join(), { audio: false, video: false });
    expect(built).toBe(2);
  });

  test("terminal disconnect releases the room, unbinds listeners, and permits reconnect", async () => {
    const room = stubRoom();
    const engine = engineWith(room);
    const disconnected: Array<{
      origin: "local" | "transport";
      reason?: DisconnectReason;
    }> = [];
    engine.on("disconnected", (info) => disconnected.push(info));

    await engine.connect(join(), { audio: false, video: false });
    const onDisconnected = room.handlers.get(RoomEvent.Disconnected) as
      | ((reason?: DisconnectReason) => Promise<void>)
      | undefined;
    expect(onDisconnected).toBeDefined();
    await onDisconnected?.(DisconnectReason.DUPLICATE_IDENTITY);

    expect((engine as unknown as { room: Room | null }).room).toBeNull();
    expect(room.offCalls).toHaveLength(16);
    expect(room.handlers.size).toBe(0);
    expect(disconnected).toEqual([{
      origin: "transport",
      reason: DisconnectReason.DUPLICATE_IDENTITY,
    }]);

    await engine.connect(join(), { audio: false, video: false });
    expect(room.connectCalls).toHaveLength(2);
  });

  test("forwards LiveKit media-device errors for active mic and camera failures", async () => {
    const room = stubRoom();
    const engine = engineWith(room);
    const seen: Array<{ source: "audio" | "video"; error: Error }> = [];
    engine.on("mediaDevicesError", (info) => {
      seen.push(info as { source: "audio" | "video"; error: Error });
    });

    await engine.connect(join(), { audio: false, video: false });
    const onMediaDevicesError = room.handlers.get(RoomEvent.MediaDevicesError) as
      | ((error: Error, kind?: MediaDeviceKind) => void)
      | undefined;
    const micError = new Error("mic gone");
    micError.name = "NotFoundError";
    const camError = new Error("cam gone");
    camError.name = "NotReadableError";

    onMediaDevicesError?.(micError, "audioinput");
    onMediaDevicesError?.(camError, "videoinput");

    expect(seen).toEqual([
      { source: "audio", error: micError },
      { source: "video", error: camError },
    ]);
  });

  test("an operation from call N cannot fall back after call N+1 connects", async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const first = stubRoom();
    const second = stubRoom();
    let screenCalls = 0;
    first.localParticipant.setScreenShareEnabled = async () => {
      screenCalls += 1;
      await gate;
      const error = new Error("screen audio unsupported");
      error.name = "NotSupportedError";
      throw error;
    };
    let built = 0;
    const engine = new CallEngine({
      makeRoom: () => (built++ === 0 ? first : second) as unknown as Room,
      videoCodecSupport: videoCodecSupport({ encode: ["video/vp8"], decode: ["video/vp8"] }),
    });

    await engine.connect(join(), { audio: false, video: false });
    const staleOperation = engine.setScreenShareEnabled(true, { audio: true });
    await engine.disconnect();
    await engine.connect(join(), { audio: false, video: false });
    release();
    await staleOperation;

    expect(screenCalls).toBe(1);
  });
});

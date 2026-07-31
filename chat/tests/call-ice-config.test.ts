import { describe, expect, test } from "bun:test";
import {
  DisconnectReason,
  RoomEvent,
  type Room,
  type RoomConnectOptions,
  type RoomOptions,
} from "livekit-client";
import { CallEngine } from "../src/lib/calls/engine";
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
    expect(room.offCalls).toHaveLength(14);
    expect(room.handlers.size).toBe(0);
    expect(disconnected).toEqual([{
      origin: "transport",
      reason: DisconnectReason.DUPLICATE_IDENTITY,
    }]);

    await engine.connect(join(), { audio: false, video: false });
    expect(room.connectCalls).toHaveLength(2);
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

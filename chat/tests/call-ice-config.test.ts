import { describe, expect, test } from "bun:test";
import type { Room, RoomConnectOptions, RoomOptions } from "livekit-client";
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
  const room = {
    connectCalls,
    on() {
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
    },
    remoteParticipants: new Map(),
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
});

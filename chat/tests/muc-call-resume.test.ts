import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { $callState, clearCallState, tearDownActiveCall } from "../src/lib/calls/call-store";
import {
  canResumeMucCallActivity,
  readvertiseMucCallPresence,
  resumeMucCallActivity,
} from "../src/lib/calls/muc-call-actions";
import {
  $mucCallLiveParticipants,
  setLiveCallParticipants,
} from "../src/lib/calls/muc-call-live-participants";
import {
  clearAllMucCallSessionCacheForTests,
  readMucCallSession,
  rememberMucCallSession,
} from "../src/lib/calls/muc-call-session-cache";
import type { CallMedia, LiveKitJoin } from "../src/lib/calls/types";

/**
 * Forge a LiveKit join JWT whose `exp` claim sits `secondsFromNow`
 * ahead of the current wall clock. The signature is irrelevant for
 * the chat-side expiry check — only `exp` is read — so the test
 * uses a placeholder signature.
 */
function forgeLiveKitJoin(opts: {
  identity: string;
  secondsFromNow: number;
}): LiveKitJoin {
  const header = base64UrlEncodeJson({ alg: "HS256", typ: "JWT" });
  const exp = Math.floor(Date.now() / 1000) + opts.secondsFromNow;
  const payload = base64UrlEncodeJson({ sub: opts.identity, exp });
  const token = `${header}.${payload}.dGVzdC1zaWduYXR1cmU`;
  return {
    url: "wss://livekit.test",
    room: "general@muc.test",
    identity: opts.identity,
    token,
  };
}

function base64UrlEncodeJson(value: unknown): string {
  const json = JSON.stringify(value);
  const bytes = new TextEncoder().encode(json);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return globalThis
    .btoa(binary)
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

const SELF_FULL_JID = "alice@waddle.test/web";
const ROOM_JID = "general@muc.test";
const SELF_NICK = "alice";
const AUDIO_CALL: CallMedia = { audio: true, video: false };

const WINDOW_SENTINEL = Symbol("muc-call-resume-window");
type ShimmedGlobal = typeof globalThis & {
  window?: { localStorage: Storage; sessionStorage: Storage } & { [WINDOW_SENTINEL]?: true };
};

beforeAll(() => {
  const g = globalThis as ShimmedGlobal;
  if (typeof g.window !== "undefined") return;
  const storage = createStorage();
  const sessionStorage = createStorage();
  g.window = Object.assign({ localStorage: storage, sessionStorage }, { [WINDOW_SENTINEL]: true as const });
});

afterAll(() => {
  const g = globalThis as ShimmedGlobal;
  if (g.window?.[WINDOW_SENTINEL]) {
    delete (g as { window?: unknown }).window;
  }
});

function createStorage(): Storage {
  const store = new Map<string, string>();
  return {
    get length() {
      return store.size;
    },
    clear: () => store.clear(),
    getItem: (key) => store.get(key) ?? null,
    key: (index) => Array.from(store.keys())[index] ?? null,
    removeItem: (key) => {
      store.delete(key);
    },
    setItem: (key, value) => {
      store.set(key, String(value));
    },
  };
}

beforeEach(() => {
  clearAllMucCallSessionCacheForTests();
  clearCallState();
});

afterEach(() => {
  clearAllMucCallSessionCacheForTests();
  clearCallState();
});

describe("canResumeMucCallActivity", () => {
  test("returns false when no cached session exists", () => {
    expect(
      canResumeMucCallActivity({
        roomJid: ROOM_JID,
        selfFullJid: SELF_FULL_JID,
      }),
    ).toBe(false);
  });

  test("returns false when cached session has no join (terminate-only entry)", () => {
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "sid-1",
      selfFullJid: SELF_FULL_JID,
      media: AUDIO_CALL,
    });
    expect(
      canResumeMucCallActivity({
        roomJid: ROOM_JID,
        selfFullJid: SELF_FULL_JID,
      }),
    ).toBe(false);
  });

  test("returns true when cached join is fresh and identity matches", () => {
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "sid-2",
      selfFullJid: SELF_FULL_JID,
      media: AUDIO_CALL,
      join: forgeLiveKitJoin({ identity: SELF_FULL_JID, secondsFromNow: 600 }),
    });
    expect(
      canResumeMucCallActivity({
        roomJid: ROOM_JID,
        selfFullJid: SELF_FULL_JID,
      }),
    ).toBe(true);
  });

  test("returns false when the cached join's identity is for a different resource", () => {
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "sid-3",
      selfFullJid: SELF_FULL_JID,
      media: AUDIO_CALL,
      // Cached join was minted for a dead pre-reload resource. The
      // current XMPP session bound under a new resource cannot reuse
      // it directly — falling back to beginMucCall is correct.
      join: forgeLiveKitJoin({
        identity: "alice@waddle.test/desktop-dead",
        secondsFromNow: 600,
      }),
    });
    expect(
      canResumeMucCallActivity({
        roomJid: ROOM_JID,
        selfFullJid: SELF_FULL_JID,
      }),
    ).toBe(false);
  });

  test("returns false when the cached JWT's exp is within the skew window", () => {
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "sid-4",
      selfFullJid: SELF_FULL_JID,
      media: AUDIO_CALL,
      // 10 seconds — inside the 30-second LIVEKIT_JOIN_EXPIRY_SKEW_MS
      // guard. Resume must refuse so a token that's about to lapse
      // doesn't race the reconnect.
      join: forgeLiveKitJoin({ identity: SELF_FULL_JID, secondsFromNow: 10 }),
    });
    expect(
      canResumeMucCallActivity({
        roomJid: ROOM_JID,
        selfFullJid: SELF_FULL_JID,
      }),
    ).toBe(false);
  });
});

describe("resumeMucCallActivity", () => {
  test("returns false and leaves $callState idle when no cached session exists", async () => {
    const ok = await resumeMucCallActivity({
      roomJid: ROOM_JID,
      getSender: () => null,
      getSelfNick: () => SELF_NICK,
      getSelfFullJid: () => SELF_FULL_JID,
    });
    expect(ok).toBe(false);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("promotes $callState directly to active with the cached join", async () => {
    const join = forgeLiveKitJoin({
      identity: SELF_FULL_JID,
      secondsFromNow: 600,
    });
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "sid-resume",
      selfFullJid: SELF_FULL_JID,
      media: AUDIO_CALL,
      join,
    });

    const ok = await resumeMucCallActivity({
      roomJid: ROOM_JID,
      // No sender exposed — best-effort Muji republish should
      // skip silently rather than fail the resume.
      getSender: () => null,
      getSelfNick: () => SELF_NICK,
      getSelfFullJid: () => SELF_FULL_JID,
    });

    expect(ok).toBe(true);
    const state = $callState.get();
    expect(state.phase).toBe("active");
    if (state.phase !== "active") return;
    expect(state.kind).toBe("muc");
    if (state.kind !== "muc") return;
    expect(state.peer).toBe(ROOM_JID);
    expect(state.sid).toBe("sid-resume");
    expect(state.join).toEqual(join);
    expect(state.selfNick).toBe(SELF_NICK);
    expect(state.selfFullJid).toBe(SELF_FULL_JID);
  });

  test("republishes Muji active presence when a sender is available", async () => {
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "sid-republish",
      selfFullJid: SELF_FULL_JID,
      media: { audio: true, video: true },
      join: forgeLiveKitJoin({
        identity: SELF_FULL_JID,
        secondsFromNow: 600,
      }),
    });

    const calls: Array<{
      roomJid: string;
      nick: string;
      active: boolean;
      preparing: boolean;
      video: boolean;
    }> = [];
    const ok = await resumeMucCallActivity({
      roomJid: ROOM_JID,
      getSender: () => ({
        update_muji_presence: async (roomJid, nick, active, preparing, video) => {
          calls.push({ roomJid, nick, active, preparing, video });
        },
      }),
      getSelfNick: () => SELF_NICK,
      getSelfFullJid: () => SELF_FULL_JID,
    });

    expect(ok).toBe(true);
    expect(calls).toEqual([
      {
        roomJid: ROOM_JID,
        nick: SELF_NICK,
        active: true,
        preparing: false,
        video: true,
      },
    ]);
  });

  test("readvertiseMucCallPresence re-emits the full advertisement for the active call", async () => {
    // A fresh XMPP bind wiped the server-side occupant; the plain rejoin
    // presence carries no <muji/>, so the still-running call must
    // re-advertise (with current hand/mute markers) once the room join
    // confirms (#1621 round 2).
    $callState.set({
      phase: "active",
      peer: ROOM_JID,
      sid: "sid-readvertise",
      media: { audio: true, video: true },
      join: forgeLiveKitJoin({ identity: SELF_FULL_JID, secondsFromNow: 600 }),
      kind: "muc",
      selfNick: SELF_NICK,
      selfFullJid: SELF_FULL_JID,
    });
    const calls: Array<{
      roomJid: string;
      nick: string;
      active: boolean;
      video: boolean;
      flags: unknown;
    }> = [];

    const ok = await readvertiseMucCallPresence({
      update_muji_presence: async (roomJid, nick, active, _preparing, video, flags) => {
        calls.push({ roomJid, nick, active, video, flags });
      },
    });

    expect(ok).toBe(true);
    expect(calls).toEqual([
      {
        roomJid: ROOM_JID,
        nick: SELF_NICK,
        active: true,
        video: true,
        flags: { handRaised: false, muted: false },
      },
    ]);
  });

  test("readvertiseMucCallPresence is a no-op without an active MUC call", async () => {
    clearCallState();
    const ok = await readvertiseMucCallPresence({
      update_muji_presence: async () => {
        throw new Error("must not be called");
      },
    });
    expect(ok).toBe(false);
  });

  test("refuses to resume when $callState is not idle", async () => {
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "sid-busy",
      selfFullJid: SELF_FULL_JID,
      media: AUDIO_CALL,
      join: forgeLiveKitJoin({
        identity: SELF_FULL_JID,
        secondsFromNow: 600,
      }),
    });
    // Pretend an outgoing call is already in flight.
    $callState.set({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "other-call",
      media: AUDIO_CALL,
    });

    const ok = await resumeMucCallActivity({
      roomJid: ROOM_JID,
      getSender: () => null,
      getSelfNick: () => SELF_NICK,
      getSelfFullJid: () => SELF_FULL_JID,
    });

    expect(ok).toBe(false);
    expect($callState.get().phase).toBe("outgoing");
  });

  test("reload resume then hangup clears session cache and live participants", async () => {
    const join = forgeLiveKitJoin({
      identity: SELF_FULL_JID,
      secondsFromNow: 600,
    });
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "sid-resume-hangup",
      selfFullJid: SELF_FULL_JID,
      media: AUDIO_CALL,
      join,
    });

    const resumed = await resumeMucCallActivity({
      roomJid: ROOM_JID,
      getSender: () => ({
        update_muji_presence: async () => undefined,
        send_muji_session_terminate: async () => undefined,
      }),
      getSelfNick: () => SELF_NICK,
      getSelfFullJid: () => SELF_FULL_JID,
    });
    expect(resumed).toBe(true);

    setLiveCallParticipants(ROOM_JID, [SELF_FULL_JID, "bob@waddle.test/mobile"]);

    await tearDownActiveCall(
      {
        update_muji_presence: async () => undefined,
        send_muji_session_terminate: async () => undefined,
      } as unknown as Parameters<typeof tearDownActiveCall>[0],
      "success",
    );

    expect($callState.get()).toEqual({ phase: "idle" });
    expect(readMucCallSession({
      roomJid: ROOM_JID,
      selfFullJid: SELF_FULL_JID,
    })).toBeNull();
    expect($mucCallLiveParticipants.get()[ROOM_JID]).toBeUndefined();
  });
});

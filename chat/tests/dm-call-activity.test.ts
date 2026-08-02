import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import {
  $dmCallActivities,
  DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS,
  applyDmCallEvent,
  clearDmCallActivities,
  clearDmCallActivity,
  dmCallResumeBlockReason,
  pruneExpiredDmCallActivities,
  readDmCallActivity,
  validateLiveKitGrant,
} from "../src/lib/calls/dm-call-activity";
import { clearAllDmCallJoinCacheForTests } from "../src/lib/calls/dm-call-join-cache";
import type { CallEvent } from "../src/lib/calls/types";

const self = "alice@waddle.test";
const bob = "bob@waddle.test";
const audio = { audio: true, video: false };
const now = new Date("2026-05-25T10:00:00.000Z");
const WINDOW_SENTINEL = Symbol("dm-call-activity-window");
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

function jwtWithExp(exp: number): string {
  return jwtWithPayload({ exp });
}

function jwtWithPayload(payload: Record<string, unknown>): string {
  return [
    base64UrlJson({ alg: "none", typ: "JWT" }),
    base64UrlJson(payload),
    "sig",
  ].join(".");
}

function base64UrlJson(value: Record<string, unknown>): string {
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

function createStorage(): Storage {
  const store = new Map<string, string>();
  return {
    get length() { return store.size; },
    clear: () => store.clear(),
    getItem: (key) => store.get(key) ?? null,
    key: (index) => Array.from(store.keys())[index] ?? null,
    removeItem: (key) => { store.delete(key); },
    setItem: (key, value) => { store.set(key, String(value)); },
  };
}

describe("DM call activity", () => {
  beforeEach(() => {
    clearDmCallActivities();
    clearAllDmCallJoinCacheForTests();
  });

  afterEach(() => {
    clearDmCallActivities();
    clearAllDmCallJoinCacheForTests();
  });

  test("tracks an unresolved remote proposal as ringing activity", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "call-1",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: now.toISOString(),
      now,
    });

    expect(readDmCallActivity(bob, now)).toEqual({
      peerJid: bob,
      remoteFullJid: `${bob}/phone`,
      sid: "call-1",
      media: audio,
      state: "ringing",
      direction: "incoming",
      updatedAt: now.toISOString(),
    });
  });

  test("uses the to JID for self-sent carbon events from another resource", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/phone`,
        to: `${bob}/desktop`,
        sid: "call-2",
        media: { audio: true, video: true },
      },
      selfBareJid: self,
      timestamp: now.toISOString(),
      now,
    });

    expect(readDmCallActivity(bob, now)?.direction).toBe("outgoing");
    expect(readDmCallActivity(bob, now)?.media.video).toBe(true);
  });

  test("marks a call accepted when a peer proceeds on one resource", () => {
    const propose: CallEvent = {
      kind: "propose",
      from: `${self}/web`,
      sid: "call-3",
      media: audio,
    };
    applyDmCallEvent({
      event: propose,
      selfBareJid: self,
      to: bob,
      timestamp: now.toISOString(),
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "call-3",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:00:10.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      peerJid: bob,
      sid: "call-3",
      media: audio,
      state: "accepted",
      direction: "outgoing",
    });
  });

  test("removes activity when finish arrives for the same sid", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "call-4",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: now.toISOString(),
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${bob}/phone`,
        sid: "call-4",
        reason: "success",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
  });

  test("self-sent terminal events can clear by sid when the peer hint is gone", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/web`,
        to: `${bob}/phone`,
        sid: "call-6",
        media: audio,
      },
      selfBareJid: self,
      timestamp: now.toISOString(),
      now,
    });
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "call-6",
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:00:10.000Z",
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${self}/other`,
        sid: "call-6",
        reason: "success",
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
  });

  test("older MAM call events cannot regress a newer activity state", () => {
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "call-7",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "call-7",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      sid: "call-7",
      state: "accepted",
      updatedAt: "2026-05-25T10:05:00.000Z",
    });
  });

  test("does not treat proceed-only accepted call media as known voice", () => {
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "call-without-propose",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: now.toISOString(),
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      sid: "call-without-propose",
      state: "accepted",
      mediaKnown: false,
    });
  });

  test("stores LiveKit join credentials from archived accepted call events", () => {
    const join = {
      url: "wss://livekit.waddle.test",
      room: "dm-call-joined",
      identity: `${self}/web`,
      token: "token",
    };

    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "call-with-join",
        media: { audio: true, video: true },
        join,
      },
      selfBareJid: self,
      timestamp: now.toISOString(),
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      peerJid: bob,
      remoteFullJid: `${bob}/phone`,
      sid: "call-with-join",
      media: { audio: true, video: true },
      state: "accepted",
      join,
    });
  });

  test("does not carry archived LiveKit join credentials across a newer sid", () => {
    const join = {
      url: "wss://livekit.waddle.test",
      room: "dm-call-old",
      identity: `${self}/web`,
      token: "token",
    };

    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "old-sid",
        media: { audio: true, video: true },
        join,
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "new-sid",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:01:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      sid: "new-sid",
      state: "accepted",
      mediaKnown: false,
    });
    expect(readDmCallActivity(bob, now)?.join).toBeUndefined();
  });

  test("does not carry archived LiveKit join credentials into a newer proposal", () => {
    const join = {
      url: "wss://livekit.waddle.test",
      room: "dm-call-old",
      identity: `${self}/web`,
      token: "token",
    };

    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "old-sid",
        media: { audio: true, video: true },
        join,
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/tablet`,
        sid: "new-sid",
        media: audio,
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:01:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      sid: "new-sid",
      state: "ringing",
      media: audio,
    });
    expect(readDmCallActivity(bob, now)?.join).toBeUndefined();
  });

  test("restores same-resource cached LiveKit joins when refresh MAM replays only message markers", () => {
    const join = {
      url: "wss://livekit.waddle.test",
      room: "dm-call-cached",
      identity: `${self}/web`,
      token: jwtWithExp(Math.floor(now.getTime() / 1000) + 3600),
    };

    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "cached-sid",
        media: { audio: true, video: true },
        join,
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });
    clearDmCallActivities();

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/web`,
        to: `${bob}/phone`,
        sid: "cached-sid",
        media: { audio: true, video: true },
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:00:01.000Z",
      now,
    });
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "cached-sid",
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:00:02.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      sid: "cached-sid",
      state: "accepted",
      remoteFullJid: `${bob}/phone`,
      media: { audio: true, video: true },
      join,
    });
  });

  test("parses LiveKit token expiry from UTF-8 JWT payloads", () => {
    const join = {
      url: "wss://livekit.waddle.test",
      room: "dm-call-unicode-token",
      identity: `${self}/web`,
      token: jwtWithPayload({
        exp: Math.floor(now.getTime() / 1000) + 3600,
        display: "Álice 😀",
      }),
    };

    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "unicode-token",
        media: audio,
        join,
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: now.toISOString(),
      now,
    });

    const activity = readDmCallActivity(bob, now);
    expect(activity).not.toBeNull();
    expect(dmCallResumeBlockReason(activity!, `${self}/web`, now)).toBeNull();
  });

  test("does not restore cached LiveKit joins for another browser resource", () => {
    const join = {
      url: "wss://livekit.waddle.test",
      room: "dm-call-cached",
      identity: `${self}/web`,
      token: jwtWithExp(Math.floor(now.getTime() / 1000) + 3600),
    };

    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "cached-sid",
        media: audio,
        join,
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });
    clearDmCallActivities();

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/tablet`,
        to: `${bob}/phone`,
        sid: "cached-sid",
        media: audio,
      },
      selfBareJid: self,
      selfFullJid: `${self}/tablet`,
      timestamp: "2026-05-25T10:00:01.000Z",
      now,
    });
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        to: `${self}/tablet`,
        sid: "cached-sid",
      },
      selfBareJid: self,
      selfFullJid: `${self}/tablet`,
      timestamp: "2026-05-25T10:00:02.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      sid: "cached-sid",
      state: "accepted",
    });
    expect(readDmCallActivity(bob, now)?.join).toBeUndefined();
  });

  test("terminal call events clear cached LiveKit joins before a same-sid replay", () => {
    const join = {
      url: "wss://livekit.waddle.test",
      room: "dm-call-cached",
      identity: `${self}/web`,
      token: jwtWithExp(Math.floor(now.getTime() / 1000) + 3600),
    };

    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "cached-terminal-sid",
        media: { audio: true, video: true },
        join,
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });
    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "cached-terminal-sid",
        reason: "success",
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:01:00.000Z",
      now,
    });
    clearDmCallActivities();

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/web`,
        to: `${bob}/phone`,
        sid: "cached-terminal-sid",
        media: { audio: true, video: true },
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:02:00.000Z",
      now,
    });
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        to: `${self}/web`,
        sid: "cached-terminal-sid",
      },
      selfBareJid: self,
      selfFullJid: `${self}/web`,
      timestamp: "2026-05-25T10:02:01.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      sid: "cached-terminal-sid",
      state: "accepted",
    });
    expect(readDmCallActivity(bob, now)?.join).toBeUndefined();
  });

  test("terminal MAM events prevent older proposals for the same sid from resurrecting activity", () => {
    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${bob}/phone`,
        sid: "call-8",
        reason: "success",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "call-8",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
  });

  test("self-sent terminal events with a to peer tombstone older sibling proposals", () => {
    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${self}/phone`,
        to: `${bob}/desktop`,
        sid: "call-9",
        reason: "success",
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/phone`,
        to: `${bob}/desktop`,
        sid: "call-9",
        media: audio,
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
  });

  test("does not let 24h-old catch-up proposals resurrect old calls", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "old-call",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-24T09:59:59.000Z",
      now,
    });

    expect($dmCallActivities.get()).toEqual({});
  });

  test("does not let 24h-old accepted catch-up events resurrect old calls", () => {
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "old-call",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-24T09:59:59.000Z",
      now,
    });

    expect($dmCallActivities.get()).toEqual({});
  });

  test("prunes visible unresolved activity after the 24h XEP-0353 fallback window", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "aging-call",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: now.toISOString(),
      now,
    });

    expect(readDmCallActivity(bob, now)?.sid).toBe("aging-call");

    pruneExpiredDmCallActivities(new Date("2026-05-26T10:00:01.000Z"));

    expect(readDmCallActivity(bob, new Date("2026-05-26T10:00:01.000Z"))).toBeNull();
  });

  test("scheduled prune timer clears subscribed activity after the fallback window", async () => {
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const timer = { unref: () => undefined } as unknown as ReturnType<typeof setTimeout>;
    let scheduledPrune: (() => void) | null = null;
    let scheduledDelay: number | undefined;

    globalThis.setTimeout = ((handler: TimerHandler, timeout?: number, ...args: unknown[]) => {
      scheduledDelay = Number(timeout ?? 0);
      scheduledPrune = () => {
        if (typeof handler === "function") {
          handler(...args);
        }
      };
      return timer;
    }) as typeof setTimeout;
    globalThis.clearTimeout = (() => {
      scheduledPrune = null;
    }) as typeof clearTimeout;

    try {
      const timerArmedAt = new Date();
      const timestamp = new Date(timerArmedAt.getTime() - DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS).toISOString();
      applyDmCallEvent({
        event: {
          kind: "propose",
          from: `${bob}/phone`,
          sid: "timer-pruned-call",
          media: audio,
        },
        selfBareJid: self,
        to: `${self}/web`,
        timestamp,
        now: timerArmedAt,
      });

      expect(readDmCallActivity(bob, timerArmedAt)?.sid).toBe("timer-pruned-call");
      expect(scheduledDelay).toBe(1_000);
      expect(scheduledPrune).not.toBeNull();

      await new Promise<void>((resolve) => originalSetTimeout(resolve, 5));
      scheduledPrune?.();

      expect($dmCallActivities.get()).toEqual({});
    } finally {
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
    }
  });

  test("scheduled expiry timer refreshes accepted call affordances", async () => {
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const timer = { unref: () => undefined } as unknown as ReturnType<typeof setTimeout>;
    let scheduledRefresh: (() => void) | null = null;
    let scheduledDelay: number | undefined;

    globalThis.setTimeout = ((handler: TimerHandler, timeout?: number, ...args: unknown[]) => {
      scheduledDelay = Number(timeout ?? 0);
      scheduledRefresh = () => {
        if (typeof handler === "function") {
          handler(...args);
        }
      };
      return timer;
    }) as typeof setTimeout;
    globalThis.clearTimeout = (() => {
      scheduledRefresh = null;
    }) as typeof clearTimeout;

    try {
      const timerArmedAt = new Date();
      const join = {
        url: "wss://livekit.waddle.test",
        room: "dm-call-expiring",
        identity: `${self}/web`,
        token: jwtWithExp(Math.floor((timerArmedAt.getTime() + 60_000) / 1000)),
      };
      applyDmCallEvent({
        event: {
          kind: "session-accept",
          from: `${bob}/phone`,
          to: `${self}/web`,
          sid: "expiring-call",
          media: audio,
          join,
        },
        selfBareJid: self,
        selfFullJid: `${self}/web`,
        timestamp: timerArmedAt.toISOString(),
        now: timerArmedAt,
      });

      const before = $dmCallActivities.get();
      expect(scheduledDelay).toBeGreaterThanOrEqual(30_000);
      expect(scheduledDelay).toBeLessThanOrEqual(31_000);
      expect(scheduledRefresh).not.toBeNull();
      scheduledRefresh?.();

      expect($dmCallActivities.get()).not.toBe(before);
      expect(readDmCallActivity(bob)).toMatchObject({
        sid: "expiring-call",
        join,
      });
    } finally {
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
    }
  });

  test("treats unparseable activity timestamps as expired", () => {
    $dmCallActivities.set({
      [bob]: {
        peerJid: bob,
        sid: "invalid-clock-call",
        media: audio,
        state: "ringing",
        direction: "incoming",
        updatedAt: "not-a-date",
      },
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
    expect($dmCallActivities.get()).toEqual({});
  });

  test("local cleanup can clear an optimistic outgoing proposal", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/web`,
        sid: "call-5",
        media: audio,
      },
      selfBareJid: self,
      to: bob,
      timestamp: now.toISOString(),
      now,
    });

    clearDmCallActivity(bob, "call-5");

    expect(readDmCallActivity(bob, now)).toBeNull();
  });
});

describe("validateLiveKitGrant (defensive pre-connect check)", () => {
  const ROOM = "design@muc.waddle.test";

  function grantToken(video: unknown): string {
    return jwtWithPayload({ exp: 4_102_444_800, video });
  }

  test("accepts a server-minted camelCase grant with roomJoin + room", () => {
    const token = grantToken({
      roomJoin: true,
      room: ROOM,
      canPublish: true,
      canSubscribe: true,
      canPublishData: true,
    });
    expect(validateLiveKitGrant(token)).toEqual({ ok: true });
  });

  test("accepts a join-only grant (publish/subscribe flags are not required)", () => {
    expect(validateLiveKitGrant(grantToken({ roomJoin: true, room: ROOM }))).toEqual({ ok: true });
  });

  test("rejects a token whose payload segment is not decodable", () => {
    expect(validateLiveKitGrant("not-a-jwt")).toEqual({ ok: false, reason: "malformed-token" });
    expect(validateLiveKitGrant("")).toEqual({ ok: false, reason: "malformed-token" });
  });

  test("rejects a token that carries no video grant", () => {
    expect(validateLiveKitGrant(jwtWithPayload({ exp: 4_102_444_800 }))).toEqual({
      ok: false,
      reason: "missing-grant",
    });
    expect(validateLiveKitGrant(grantToken(null))).toEqual({ ok: false, reason: "missing-grant" });
  });

  test("rejects a grant that does not actually grant roomJoin", () => {
    expect(validateLiveKitGrant(grantToken({ room: ROOM }))).toEqual({
      ok: false,
      reason: "join-not-granted",
    });
    expect(validateLiveKitGrant(grantToken({ roomJoin: false, room: ROOM }))).toEqual({
      ok: false,
      reason: "join-not-granted",
    });
    // A truthy-but-not-true value (e.g. a stringified flag) must not
    // sneak past — LiveKit grants are strict booleans.
    expect(validateLiveKitGrant(grantToken({ roomJoin: "true", room: ROOM }))).toEqual({
      ok: false,
      reason: "join-not-granted",
    });
  });

  test("rejects a grant with a missing or empty room", () => {
    expect(validateLiveKitGrant(grantToken({ roomJoin: true }))).toEqual({
      ok: false,
      reason: "missing-room",
    });
    expect(validateLiveKitGrant(grantToken({ roomJoin: true, room: "   " }))).toEqual({
      ok: false,
      reason: "missing-room",
    });
  });
});

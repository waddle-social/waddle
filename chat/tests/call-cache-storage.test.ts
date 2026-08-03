import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { rememberDmCallJoin, readDmCallJoin, clearAllDmCallJoinCacheForTests } from "../src/lib/calls/dm-call-join-cache";
import {
  clearAllMucCallSessionCacheForTests,
  readMucCallSession,
  rememberMucCallSession,
} from "../src/lib/calls/muc-call-session-cache";
import type { CallMedia, LiveKitJoin } from "../src/lib/calls/types";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";

const WINDOW_SENTINEL = Symbol("call-cache-storage-window");
const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const DM_CACHE_KEY = "waddle.chat.dm-call-joins.alice@example.com";
const MUC_CACHE_KEY = "waddle.chat.muc-call-sessions.alice@example.com";
const OTHER_DM_CACHE_KEY = "waddle.chat.dm-call-joins.bob@example.com";
const OTHER_MUC_CACHE_KEY = "waddle.chat.muc-call-sessions.bob@example.com";
const SELF_BARE_JID = "alice@example.com";
const SELF_FULL_JID = "alice@example.com/web";
const PEER_JID = "bob@example.com";
const ROOM_JID = "room@muc.example.com";
const MEDIA: CallMedia = { audio: true, video: false };
const JOIN: LiveKitJoin = {
  url: "wss://livekit.example.test",
  room: ROOM_JID,
  identity: SELF_FULL_JID,
  token: "header.payload.sig",
};

type ShimmedGlobal = typeof globalThis & {
  window?: { localStorage: Storage; sessionStorage: Storage } & { [WINDOW_SENTINEL]?: true };
};

beforeEach(() => {
  installWindowStorage();
  clearAllDmCallJoinCacheForTests();
  clearAllMucCallSessionCacheForTests();
});

afterEach(() => {
  clearAllDmCallJoinCacheForTests();
  clearAllMucCallSessionCacheForTests();
  const g = globalThis as ShimmedGlobal;
  g.window?.localStorage.clear();
  g.window?.sessionStorage.clear();
  if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

describe("call cache storage", () => {
  test("re-purges legacy localStorage tokens written after the first pass", () => {
    const g = globalThis as ShimmedGlobal;
    // First cache touch purges whatever pre-migration builds left behind.
    readDmCallJoin({ selfBareJid: SELF_BARE_JID, peerJid: PEER_JID, sid: "s1", selfFullJid: SELF_FULL_JID });
    // A still-open pre-migration tab (rolling deployment) writes another
    // bearer token into the SHARED origin-wide localStorage afterwards.
    g.window?.localStorage.setItem(DM_CACHE_KEY, JSON.stringify([{ sid: "stale" }]));
    g.window?.localStorage.setItem(MUC_CACHE_KEY, JSON.stringify([{ sid: "stale" }]));

    // The next cache access must purge it again — a one-shot guard would
    // leave the recreated token persisted for its full exp.
    readDmCallJoin({ selfBareJid: SELF_BARE_JID, peerJid: PEER_JID, sid: "s1", selfFullJid: SELF_FULL_JID });
    readMucCallSession({ roomJid: ROOM_JID, selfFullJid: SELF_FULL_JID });

    expect(g.window?.localStorage.getItem(DM_CACHE_KEY)).toBeNull();
    expect(g.window?.localStorage.getItem(MUC_CACHE_KEY)).toBeNull();
  });

  test("writes cached join tokens to sessionStorage, not localStorage", () => {
    rememberDmCallJoin({
      selfBareJid: SELF_BARE_JID,
      entry: {
        peerJid: PEER_JID,
        sid: "dm-sid",
        selfFullJid: SELF_FULL_JID,
        remoteFullJid: `${PEER_JID}/phone`,
        media: MEDIA,
        join: JOIN,
        updatedAt: "2026-07-31T09:00:00.000Z",
      },
    });
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "muc-sid",
      selfFullJid: SELF_FULL_JID,
      media: MEDIA,
      join: JOIN,
    });

    expect(window.sessionStorage.getItem(DM_CACHE_KEY)).toContain("\"token\":\"header.payload.sig\"");
    expect(window.sessionStorage.getItem(MUC_CACHE_KEY)).toContain("\"token\":\"header.payload.sig\"");
    expect(window.localStorage.getItem(DM_CACHE_KEY)).toBeNull();
    expect(window.localStorage.getItem(MUC_CACHE_KEY)).toBeNull();
  });

  test("purges legacy localStorage entries on first cache access", () => {
    window.localStorage.setItem(DM_CACHE_KEY, "[{\"legacy\":true}]");
    window.localStorage.setItem(MUC_CACHE_KEY, "[{\"legacy\":true}]");
    window.localStorage.setItem("waddle.chat.keep-me", "1");

    expect(
      readDmCallJoin({
        selfBareJid: SELF_BARE_JID,
        peerJid: PEER_JID,
        sid: "missing",
        selfFullJid: SELF_FULL_JID,
      }),
    ).toBeNull();
    expect(
      readMucCallSession({
        roomJid: ROOM_JID,
        selfFullJid: SELF_FULL_JID,
      }),
    ).toBeNull();

    expect(window.localStorage.getItem(DM_CACHE_KEY)).toBeNull();
    expect(window.localStorage.getItem(MUC_CACHE_KEY)).toBeNull();
    expect(window.localStorage.getItem("waddle.chat.keep-me")).toBe("1");
  });

  test("disconnect clears both call caches for the logged-out account", async () => {
    rememberDmCallJoin({
      selfBareJid: SELF_BARE_JID,
      entry: {
        peerJid: PEER_JID,
        sid: "dm-alice",
        selfFullJid: SELF_FULL_JID,
        remoteFullJid: `${PEER_JID}/phone`,
        media: MEDIA,
        join: JOIN,
        updatedAt: "2026-07-31T09:00:00.000Z",
      },
    });
    rememberDmCallJoin({
      selfBareJid: "bob@example.com",
      entry: {
        peerJid: "carol@example.com",
        sid: "dm-bob",
        selfFullJid: "bob@example.com/web",
        remoteFullJid: "carol@example.com/phone",
        media: MEDIA,
        join: {
          ...JOIN,
          identity: "bob@example.com/web",
        },
        updatedAt: "2026-07-31T09:05:00.000Z",
      },
    });
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "muc-alice",
      selfFullJid: SELF_FULL_JID,
      media: MEDIA,
      join: JOIN,
    });
    rememberMucCallSession({
      roomJid: ROOM_JID,
      sid: "muc-bob",
      selfFullJid: "bob@example.com/web",
      media: MEDIA,
      join: {
        ...JOIN,
        identity: "bob@example.com/web",
      },
    });

    const client = new BrowserXmppClient(session());
    await client.disconnect();

    expect(window.sessionStorage.getItem(DM_CACHE_KEY)).toBeNull();
    expect(window.sessionStorage.getItem(MUC_CACHE_KEY)).toBeNull();
    expect(window.sessionStorage.getItem(OTHER_DM_CACHE_KEY)).not.toBeNull();
    expect(window.sessionStorage.getItem(OTHER_MUC_CACHE_KEY)).not.toBeNull();
  });
});

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/web",
    session_id: "session-id",
    xmpp_websocket_url: "wss://xmpp.example.com/ws",
  } as WaddleSession;
}

function installWindowStorage(): void {
  const g = globalThis as ShimmedGlobal;
  if (!g.window?.[WINDOW_SENTINEL]) {
    g.window = Object.assign(
      {
        localStorage: createStorage(),
        sessionStorage: createStorage(),
      },
      { [WINDOW_SENTINEL]: true as const },
    );
    return;
  }
  g.window.localStorage.clear();
  g.window.sessionStorage.clear();
}

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

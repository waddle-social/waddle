/**
 * Unit tests for the MAM paging module extracted from
 * `BrowserXmppClient` (`src/lib/xmpp/client-mam.ts`): page-to-message
 * conversion, watermark recording, and — most importantly — the
 * reconnect catch-up page-cursor handling (XEP-0313 §4.3), all
 * exercised against a fake WASM client without the full
 * `BrowserXmppClient`.
 */
import { describe, expect, test } from "bun:test";
import { TypedEventBus, type ClientEvents } from "../src/lib/xmpp/client-events";
import { MamPager, rawMessageSeenIds, type MamWasmClient } from "../src/lib/xmpp/client-mam";
import { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";
import { clearDmCallActivities, readDmCallActivity } from "../src/lib/calls/dm-call-activity";
import type { LiveDmMessage, XmppErrorEvent } from "../src/lib/xmpp/types";
import type { WasmArchivedMessage, WasmMamPage } from "../src/lib/xmpp/wasm-types";

const SELF = "alice@example.com";
const PEER = "bob@example.com";
const MUC_PM_ALICE = "room@muc.example.com/alice";
const MUC_PM_BOB = "room@muc.example.com/bob";
const CUSTOM_MUC_PM_ALICE = "room@rooms.waddle.example/alice";
const CUSTOM_MUC_PM_BOB = "room@rooms.waddle.example/bob";

function archivedDm(mamId: string, body: string, timestamp: string, from = PEER): WasmArchivedMessage {
  return {
    mam_id: mamId,
    id: `msg-${mamId}`,
    from,
    to: from === PEER ? SELF : PEER,
    body,
    message_type: "chat",
    timestamp,
  };
}

function archivedMucPm(
  mamId: string,
  occupantJid: string,
  body: string,
  timestamp: string,
): WasmArchivedMessage {
  return {
    mam_id: mamId,
    id: `msg-${mamId}`,
    from: occupantJid,
    to: SELF,
    body,
    message_type: "chat",
    timestamp,
  };
}

function page(messages: WasmArchivedMessage[], opts: { complete?: boolean } = {}): WasmMamPage {
  return {
    messages,
    ...(messages.length > 0 ? { first_id: messages[0].mam_id, last_id: messages[messages.length - 1].mam_id } : {}),
    is_complete: opts.complete ?? false,
  };
}

function createPager(
  xmpp: MamWasmClient,
  overrides: {
    currentXmpp?: () => MamWasmClient | null;
    classifyMucPm?: (message: WasmArchivedMessage) => { occupantJid: string; nick: string } | undefined;
    isMucPmPeer?: (peerJid: string) => boolean;
    catchup?: ReconnectCatchup;
  } = {},
) {
  const events = new TypedEventBus<ClientEvents>();
  const catchup = overrides.catchup ?? new ReconnectCatchup();
  const errors: XmppErrorEvent[] = [];
  const pager = new MamPager({
    sessionJid: () => SELF,
    fullJid: () => `${SELF}/web-test`,
    trustedMediaOrigin: () => null,
    currentRoom: () => null,
    catchup,
    events,
    emitError: (event) => errors.push(event),
    requireConnectedXmpp: async () => xmpp,
    ensureRoomReady: async (_spaceId, channelId) => ({
      xmpp,
      roomJid: `${channelId}@muc.example.com`,
    }),
    roomJidForChannel: (channelId) => `${channelId}@muc.example.com`,
    isCurrentConnected: (candidate) =>
      overrides.currentXmpp ? overrides.currentXmpp() === candidate : xmpp === candidate,
    // Mirrors BrowserXmppClient.mucPmOccupant for a known test room.
    classifyMucPm: overrides.classifyMucPm ?? ((message) => {
      const counterpart = (message.from ?? "").startsWith(SELF) ? (message.to ?? "") : (message.from ?? "");
      const [bare, nick] = [counterpart.split("/")[0], counterpart.split("/").slice(1).join("/")];
      if (!nick || bare !== "room@muc.example.com") return undefined;
      return { occupantJid: counterpart, nick };
    }),
    isMucPmPeer: overrides.isMucPmPeer ?? ((peerJid) => peerJid.includes("@muc.example.com/")),
  });
  return { pager, events, catchup, errors };
}

describe("MamPager page conversion", () => {
  test("queryPersonalMamPage converts messages, surfaces cursor ids, and records watermarks", async () => {
    const wasmPage = page([
      archivedDm("a1", "hello", "2024-01-01T00:00:01Z"),
      archivedDm("a2", "world", "2024-01-01T00:00:02Z"),
    ], { complete: true });
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => wasmPage,
    };
    const { pager, catchup } = createPager(xmpp);

    const result = await pager.queryPersonalMamPage(PEER, 50);

    expect(result.messages.map((message) => message.body)).toEqual(["hello", "world"]);
    expect(result.firstArchiveId).toBe("a1");
    expect(result.lastArchiveId).toBe("a2");
    expect(result.complete).toBe(true);
    expect(catchup.getDmLastSeen(PEER)).toBe("2024-01-01T00:00:02.000Z");
  });

  test("queryMamPage targets the discovered room JID", async () => {
    const calls: Array<{ roomJid: string; max: number }> = [];
    const xmpp: MamWasmClient = {
      fetch_room_history_page: async (roomJid, max) => {
        calls.push({ roomJid, max });
        return page([], { complete: true });
      },
    };
    const { pager } = createPager(xmpp);

    const result = await pager.queryMamPage("space", "general", 25);

    expect(calls).toEqual([{ roomJid: "general@muc.example.com", max: 25 }]);
    expect(result).toEqual({ messages: [], complete: true });
  });

  test("ordinary full-resource DM pages retain bare conversation behavior", async () => {
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([
          archivedDm("resource-page", "hello", "2024-01-01T00:00:01Z", `${PEER}/phone`),
        ], { complete: true });
      },
    };
    const { pager } = createPager(xmpp);

    const result = await pager.queryPersonalMamPage(`${PEER}/phone`);

    expect(requestedPeers).toEqual([PEER]);
    expect(result.messages.map((message) => message.body)).toEqual(["hello"]);
  });

  test("known MUC authority overrides a stale bare account cursor", async () => {
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([], { complete: true });
      },
    };
    const { pager, catchup } = createPager(xmpp);
    catchup.recordDmSeen("room@muc.example.com/mobile", "2026-07-01T09:00:00.000Z");
    await pager.queryPersonalMamPage(MUC_PM_ALICE);

    expect(requestedPeers).toEqual([MUC_PM_ALICE]);
  });

  test("ordinary full-resource DM thread pages retain bare conversation behavior", async () => {
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_by_thread: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([
          archivedDm("resource-thread", "thread reply", "2024-01-01T00:00:01Z", `${PEER}/phone`),
        ], { complete: true });
      },
    };
    const { pager } = createPager(xmpp);

    const result = await pager.queryPersonalMamThreadPage(`${PEER}/phone`, "thread-1");

    expect(requestedPeers).toEqual([PEER]);
    expect(result.messages.map((message) => message.body)).toEqual(["thread reply"]);
  });

  test("ordinary full-resource DM search retains bare conversation behavior", async () => {
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      search_dm_history: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([
          archivedDm("resource-search", "matching", "2024-01-01T00:00:01Z", `${PEER}/phone`),
        ], { complete: true });
      },
    };
    const { pager } = createPager(xmpp);

    const result = await pager.searchDmMessages(`${PEER}/phone`, "matching");

    expect(requestedPeers).toEqual([PEER]);
    expect(result.map((message) => message.body)).toEqual(["matching"]);
  });
});

describe("MamPager reconnect catch-up cursor handling", () => {
  test("pages forward from the persisted after-cursor until the archive reports complete", async () => {
    const cursors: Array<string | undefined> = [];
    const pages = new Map<string, WasmMamPage>([
      ["a0", page([archivedDm("a1", "one", "2024-01-01T00:00:01Z")])],
      ["a1", page([archivedDm("a2", "two", "2024-01-01T00:00:02Z")], { complete: true })],
    ]);
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (_peer, _max, pageParam) => {
        const after = pageParam.type === "after" ? pageParam.after : undefined;
        cursors.push(after);
        return pages.get(after ?? "") ?? page([], { complete: true });
      },
    };
    const { pager, events, errors } = createPager(xmpp);
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));

    await pager.runReconnectCatchup(xmpp, [{ kind: "dm", key: PEER, scope: "account", after: "a0" }]);

    expect(cursors).toEqual(["a0", "a1"]);
    expect(delivered.map((message) => message.body)).toEqual(["one", "two"]);
    expect(errors).toEqual([]);
  });

  test("a non-advancing cursor is reported as a recoverable history error, not an infinite loop", async () => {
    // The page echoes the cursor we asked after — paging cannot advance.
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => page([archivedDm("a0", "echo", "2024-01-01T00:00:01Z")]),
    };
    const { pager, errors } = createPager(xmpp);

    await pager.runReconnectCatchup(xmpp, [{ kind: "dm", key: PEER, scope: "account", after: "a0" }]);

    expect(errors).toHaveLength(1);
    expect(errors[0].kind).toBe("history");
    expect(errors[0].recoverable).toBe(true);
  });

  test("cursor-not-found (XEP-0313 §4.3.4) falls back to timestamp catch-up and skips already-seen messages", async () => {
    let afterAttempts = 0;
    const backwardPages = [
      page([
        archivedDm("b1", "before the gap", "2024-01-01T00:00:00Z"),
        archivedDm("b2", "inside the gap", "2024-01-01T00:00:02Z"),
      ], { complete: true }),
    ];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (_peer, _max, pageParam) => {
        if (pageParam.type === "after") {
          afterAttempts += 1;
          throw new Error("item-not-found: cursor expired");
        }
        return backwardPages[0];
      },
    };
    const { pager, events, errors } = createPager(xmpp);
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));

    await pager.runReconnectCatchup(xmpp, [
      { kind: "dm", key: PEER, scope: "account", after: "gone", since: "2024-01-01T00:00:01Z" },
    ]);

    expect(afterAttempts).toBe(1);
    // Only the message newer than `since` replays; the older one is skipped.
    expect(delivered.map((message) => message.body)).toEqual(["inside the gap"]);
    expect(errors).toEqual([]);
  });

  test("aborts cleanly when the WASM handle is replaced mid-pagination", async () => {
    let current: MamWasmClient | null = null;
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => {
        current = null; // simulate reconnect swapping the handle away
        return page([archivedDm("a1", "stale", "2024-01-01T00:00:01Z")]);
      },
    };
    const { pager, events } = createPager(xmpp, { currentXmpp: () => current });
    current = xmpp;
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));
    const catchupInfos: Array<{ outcome: string }> = [];
    events.on("catchup", (info) => catchupInfos.push({ outcome: info.outcome }));

    await pager.runReconnectCatchup(xmpp, [{ kind: "dm", key: PEER, scope: "account", after: "a0" }]);

    expect(delivered).toEqual([]);
    expect(catchupInfos).toEqual([{ outcome: "aborted" }]);
  });
});

describe("MUC-PM classification and archive isolation (#1256, #1281)", () => {
  test("custom MUC service scopes page, thread, search, and reconnect by full occupant", async () => {
    const mixed = page([
      archivedMucPm("custom-bob", CUSTOM_MUC_PM_BOB, "matching bob", "2026-07-01T10:00:00.000Z"),
      archivedMucPm("custom-alice", CUSTOM_MUC_PM_ALICE, "matching alice", "2026-07-01T10:00:01.000Z"),
    ], { complete: true });
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
      fetch_dm_history_by_thread: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
      search_dm_history: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
    };
    const { pager, events } = createPager(xmpp, {
      classifyMucPm: () => undefined,
      isMucPmPeer: (peerJid) => peerJid.includes("@rooms.waddle.example/"),
    });
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));

    const history = await pager.queryPersonalMamPage(CUSTOM_MUC_PM_ALICE);
    const thread = await pager.queryPersonalMamThreadPage(CUSTOM_MUC_PM_ALICE, "thread-1");
    const search = await pager.searchDmMessages(CUSTOM_MUC_PM_ALICE, "matching");
    await pager.runReconnectCatchup(
      xmpp,
      [{ kind: "dm", key: CUSTOM_MUC_PM_ALICE, scope: "muc-occupant", after: "before-gap" }],
      "fresh",
    );

    expect(requestedPeers).toEqual(Array(4).fill(CUSTOM_MUC_PM_ALICE));
    expect(history.messages.map((message) => message.body)).toEqual(["matching alice"]);
    expect(thread.messages.map((message) => message.body)).toEqual(["matching alice"]);
    expect(search.map((message) => message.body)).toEqual(["matching alice"]);
    expect(delivered.map((message) => message.body)).toEqual(["matching alice"]);
  });

  test("persisted custom occupant scope keeps page, thread, and search full before topology discovery", async () => {
    const catchup = new ReconnectCatchup();
    catchup.recordDmSeen(
      CUSTOM_MUC_PM_ALICE,
      "2026-07-01T09:00:00.000Z",
      "custom-persisted-cursor",
      [],
      "muc-occupant",
    );
    const mixed = page([
      archivedMucPm("persisted-bob", CUSTOM_MUC_PM_BOB, "matching bob", "2026-07-01T10:00:00.000Z"),
      archivedMucPm("persisted-alice", CUSTOM_MUC_PM_ALICE, "matching alice", "2026-07-01T10:00:01.000Z"),
    ], { complete: true });
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
      fetch_dm_history_by_thread: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
      search_dm_history: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
    };
    const { pager } = createPager(xmpp, {
      catchup,
      classifyMucPm: () => undefined,
      isMucPmPeer: () => false,
    });

    const history = await pager.queryPersonalMamPage(CUSTOM_MUC_PM_ALICE);
    const thread = await pager.queryPersonalMamThreadPage(CUSTOM_MUC_PM_ALICE, "thread-1");
    const search = await pager.searchDmMessages(CUSTOM_MUC_PM_ALICE, "matching");

    expect(requestedPeers).toEqual(Array(3).fill(CUSTOM_MUC_PM_ALICE));
    expect(history.messages.map((message) => message.body)).toEqual(["matching alice"]);
    expect(thread.messages.map((message) => message.body)).toEqual(["matching alice"]);
    expect(search.map((message) => message.body)).toEqual(["matching alice"]);
  });

  test("explicit restored occupant scope isolates page, thread, and search before discovery or catchup", async () => {
    const mixed = page([
      archivedMucPm("restored-bob", CUSTOM_MUC_PM_BOB, "bob secret", "2026-07-01T10:00:00.000Z"),
      archivedMucPm("restored-alice", CUSTOM_MUC_PM_ALICE, "alice secret", "2026-07-01T10:00:01.000Z"),
    ], { complete: true });
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
      fetch_dm_history_by_thread: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
      search_dm_history: async (peerJid) => { requestedPeers.push(peerJid); return mixed; },
    };
    const { pager } = createPager(xmpp, {
      classifyMucPm: () => undefined,
      isMucPmPeer: () => false,
    });

    const history = await pager.queryPersonalMamPage(
      CUSTOM_MUC_PM_ALICE,
      100,
      { type: "latest" },
      "muc-occupant",
    );
    const thread = await pager.queryPersonalMamThreadPage(
      CUSTOM_MUC_PM_ALICE,
      "thread-1",
      100,
      { type: "latest" },
      "muc-occupant",
    );
    const search = await pager.searchDmMessages(
      CUSTOM_MUC_PM_ALICE,
      "secret",
      20,
      "muc-occupant",
    );

    expect(requestedPeers).toEqual(Array(3).fill(CUSTOM_MUC_PM_ALICE));
    expect(history.messages.map((message) => message.body)).toEqual(["alice secret"]);
    expect(thread.messages.map((message) => message.body)).toEqual(["alice secret"]);
    expect(search.map((message) => message.body)).toEqual(["alice secret"]);
  });

  test("cold-reload catch-up retains custom MUC occupant scope before topology discovery", async () => {
    const catchup = new ReconnectCatchup();
    catchup.onSessionStarted();
    catchup.recordDmSeen(
      CUSTOM_MUC_PM_ALICE,
      "2026-07-01T10:00:00.000Z",
      "custom-before-gap",
      [],
      "muc-occupant",
    );
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([
          archivedMucPm("custom-cold-bob", CUSTOM_MUC_PM_BOB, "bob secret", "2026-07-01T10:00:01.000Z"),
          archivedMucPm("custom-cold-alice", CUSTOM_MUC_PM_ALICE, "alice message", "2026-07-01T10:00:02.000Z"),
        ], { complete: true });
      },
    };
    const { pager, events } = createPager(xmpp, {
      catchup,
      classifyMucPm: () => undefined,
      isMucPmPeer: () => false,
    });
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));

    await pager.runReconnectCatchup(xmpp, catchup.onSessionStarted(), "fresh");

    expect(requestedPeers).toEqual([CUSTOM_MUC_PM_ALICE]);
    expect(delivered.map((message) => message.body)).toEqual(["alice message"]);
  });

  test("overlapping handles keep custom occupant scope request-local when the old run resolves first", async () => {
    let resolveOld: ((page: WasmMamPage) => void) | undefined;
    let resolveCurrent: ((page: WasmMamPage) => void) | undefined;
    const oldXmpp: MamWasmClient = {
      fetch_dm_history_page: async () => new Promise<WasmMamPage>((resolve) => { resolveOld = resolve; }),
    };
    const currentXmpp: MamWasmClient = {
      fetch_dm_history_page: async () => new Promise<WasmMamPage>((resolve) => { resolveCurrent = resolve; }),
    };
    let current: MamWasmClient | null = oldXmpp;
    const { pager, events } = createPager(oldXmpp, {
      currentXmpp: () => current,
      classifyMucPm: () => undefined,
      isMucPmPeer: () => false,
    });
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));
    clearDmCallActivities();
    const entry = {
      kind: "dm" as const,
      key: CUSTOM_MUC_PM_ALICE,
      scope: "muc-occupant" as const,
      after: "before-gap",
    };

    const oldRun = pager.runReconnectCatchup(oldXmpp, [entry], "fresh");
    await Promise.resolve();
    current = currentXmpp;
    const currentRun = pager.runReconnectCatchup(currentXmpp, [entry], "fresh");
    await Promise.resolve();
    resolveOld?.(page([], { complete: true }));
    await oldRun;
    const targetCall = archivedMucPm("overlap-call", CUSTOM_MUC_PM_ALICE, "", new Date().toISOString());
    targetCall.call_event = {
      kind: "propose",
      from: CUSTOM_MUC_PM_ALICE,
      to: `${SELF}/web-test`,
      sid: "overlap-call",
      media: { audio: true, video: false },
    };
    resolveCurrent?.(page([
      archivedMucPm("overlap-bob", CUSTOM_MUC_PM_BOB, "bob secret", "2026-07-01T10:00:01.000Z"),
      targetCall,
      archivedMucPm("overlap-alice", CUSTOM_MUC_PM_ALICE, "alice message", "2026-07-01T10:00:02.000Z"),
    ], { complete: true }));
    await currentRun;

    expect(delivered.map((message) => message.body)).toEqual(["alice message"]);
    expect(readDmCallActivity("room@rooms.waddle.example")).toBeNull();
    clearDmCallActivities();
  });

  test("muc-looking ordinary account resource stays a bare DM when service authority differs", async () => {
    const requestedPeers: string[] = [];
    const ordinaryPeer = "user@muc.example.com/phone";
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([archivedDm("muc-looking-user", "hello", "2026-07-01T10:00:00.000Z", ordinaryPeer)], { complete: true });
      },
    };
    const { pager } = createPager(xmpp, {
      isMucPmPeer: (peerJid) => peerJid.includes("@rooms.waddle.example/"),
    });

    const result = await pager.queryPersonalMamPage(ordinaryPeer);

    expect(requestedPeers).toEqual(["user@muc.example.com"]);
    expect(result.messages.map((message) => message.body)).toEqual(["hello"]);
  });
  test("full-peer pages isolate and re-key incoming and outgoing rows without room discovery", async () => {
    const outgoingTarget = archivedMucPm(
      "pm-alice-outgoing",
      `${SELF}/web-test`,
      "outgoing alice",
      "2026-07-01T10:00:02.000Z",
    );
    outgoingTarget.to = MUC_PM_ALICE;
    const outgoingSibling = archivedMucPm(
      "pm-bob-outgoing",
      `${SELF}/web-test`,
      "outgoing bob",
      "2026-07-01T10:00:03.000Z",
    );
    outgoingSibling.to = MUC_PM_BOB;
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => page([
        archivedMucPm("pm-bob", MUC_PM_BOB, "incoming bob", "2026-07-01T10:00:00.000Z"),
        archivedMucPm("pm-alice", MUC_PM_ALICE, "incoming alice", "2026-07-01T10:00:01.000Z"),
        outgoingSibling,
        outgoingTarget,
      ], { complete: true }),
    };
    const { pager, catchup } = createPager(xmpp, { classifyMucPm: () => undefined });

    const result = await pager.queryPersonalMamPage(MUC_PM_ALICE);

    expect(result.messages.map(({ body, peerJid, mucPm }) => ({ body, peerJid, mucPm }))).toEqual([
      { body: "incoming alice", peerJid: MUC_PM_ALICE, mucPm: true },
      { body: "outgoing alice", peerJid: MUC_PM_ALICE, mucPm: true },
    ]);
    expect(catchup.getDmLastSeen(MUC_PM_ALICE)).toBe("2026-07-01T10:00:02.000Z");
    expect(catchup.getDmLastSeen(MUC_PM_BOB)).toBeUndefined();
  });

  test("full-peer thread and search results isolate rows without room discovery", async () => {
    const mixed = page([
      archivedMucPm("result-bob", MUC_PM_BOB, "matching bob", "2026-07-01T10:00:00.000Z"),
      archivedMucPm("result-alice", MUC_PM_ALICE, "matching alice", "2026-07-01T10:00:01.000Z"),
    ], { complete: true });
    const xmpp: MamWasmClient = {
      fetch_dm_history_by_thread: async () => mixed,
      search_dm_history: async () => mixed,
    };
    const { pager } = createPager(xmpp, { classifyMucPm: () => undefined });

    const thread = await pager.queryPersonalMamThreadPage(MUC_PM_ALICE, "thread-1");
    const search = await pager.searchDmMessages(MUC_PM_ALICE, "matching");

    expect(thread.messages.map((message) => message.body)).toEqual(["matching alice"]);
    expect(search.map((message) => message.body)).toEqual(["matching alice"]);
    expect(thread.messages[0]?.peerJid).toBe(MUC_PM_ALICE);
    expect(search[0]?.peerJid).toBe(MUC_PM_ALICE);
  });

  test("full-peer reconnect emits incoming and outgoing target rows without room discovery", async () => {
    const outgoingTarget = archivedMucPm(
      "catchup-alice-outgoing",
      `${SELF}/web-test`,
      "outgoing alice",
      "2026-07-01T10:00:02.000Z",
    );
    outgoingTarget.to = MUC_PM_ALICE;
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => page([
        archivedMucPm("catchup-bob", MUC_PM_BOB, "incoming bob", "2026-07-01T10:00:00.000Z"),
        archivedMucPm("catchup-alice", MUC_PM_ALICE, "incoming alice", "2026-07-01T10:00:01.000Z"),
        outgoingTarget,
      ], { complete: true }),
    };
    const { pager, events, catchup } = createPager(xmpp, { classifyMucPm: () => undefined });
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));

    await pager.runReconnectCatchup(
      xmpp,
      [{ kind: "dm", key: MUC_PM_ALICE, scope: "muc-occupant", after: "before-gap" }],
      "fresh",
    );

    expect(delivered.map(({ body, peerJid, mucPm }) => ({ body, peerJid, mucPm }))).toEqual([
      { body: "incoming alice", peerJid: MUC_PM_ALICE, mucPm: true },
      { body: "outgoing alice", peerJid: MUC_PM_ALICE, mucPm: true },
    ]);
    expect(catchup.getDmLastSeen(MUC_PM_ALICE)).toBe("2026-07-01T10:00:02.000Z");
    expect(catchup.getDmLastSeen(MUC_PM_BOB)).toBeUndefined();
  });

  test("legacy bare-room catch-up rejects occupant rows when room discovery is unavailable", async () => {
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => page([
        archivedMucPm("legacy-bob", MUC_PM_BOB, "bob secret", "2026-07-01T10:00:00.000Z"),
        archivedMucPm("legacy-alice", MUC_PM_ALICE, "alice secret", "2026-07-01T10:00:01.000Z"),
      ], { complete: true }),
    };
    const { pager, events, catchup } = createPager(xmpp, { classifyMucPm: () => undefined });
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));

    await pager.runReconnectCatchup(
      xmpp,
      [{ kind: "dm", key: "room@muc.example.com", scope: "account", after: "legacy-boundary" }],
      "fresh",
    );

    expect(delivered).toEqual([]);
    expect(catchup.getDmLastSeen("room@muc.example.com")).toBeUndefined();
  });

  test("legacy bare-room pages reject occupant rows before conversion", async () => {
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => page([
        archivedMucPm("legacy-page-bob", MUC_PM_BOB, "bob secret", "2026-07-01T10:00:00.000Z"),
        archivedMucPm("legacy-page-alice", MUC_PM_ALICE, "alice secret", "2026-07-01T10:00:01.000Z"),
      ], { complete: false }),
    };
    const { pager, catchup } = createPager(xmpp, { classifyMucPm: () => undefined });

    const result = await pager.queryPersonalMamPage("room@muc.example.com");

    expect(result.messages).toEqual([]);
    expect(result.firstArchiveId).toBe("legacy-page-bob");
    expect(result.lastArchiveId).toBe("legacy-page-alice");
    expect(catchup.getDmLastSeen("room@muc.example.com")).toBeUndefined();
  });

  test("legacy bare-room thread pages reject occupant rows before conversion", async () => {
    const xmpp: MamWasmClient = {
      fetch_dm_history_by_thread: async () => page([
        archivedMucPm("legacy-thread-bob", MUC_PM_BOB, "bob secret", "2026-07-01T10:00:00.000Z"),
        archivedMucPm("legacy-thread-alice", MUC_PM_ALICE, "alice secret", "2026-07-01T10:00:01.000Z"),
      ], { complete: false }),
    };
    const { pager, catchup } = createPager(xmpp, { classifyMucPm: () => undefined });

    const result = await pager.queryPersonalMamThreadPage("room@muc.example.com", "thread-1");

    expect(result.messages).toEqual([]);
    expect(result.firstArchiveId).toBe("legacy-thread-bob");
    expect(result.lastArchiveId).toBe("legacy-thread-alice");
    expect(catchup.getDmLastSeen("room@muc.example.com")).toBeUndefined();
  });

  test("legacy bare-room search rejects occupant rows before conversion", async () => {
    const xmpp: MamWasmClient = {
      search_dm_history: async () => page([
        archivedMucPm("legacy-search-bob", MUC_PM_BOB, "matching bob", "2026-07-01T10:00:00.000Z"),
        archivedMucPm("legacy-search-alice", MUC_PM_ALICE, "matching alice", "2026-07-01T10:00:01.000Z"),
      ], { complete: true }),
    };
    const { pager } = createPager(xmpp, { classifyMucPm: () => undefined });

    const result = await pager.searchDmMessages("room@muc.example.com", "matching");

    expect(result).toEqual([]);
  });

  test("full-peer pages ignore target call events and do not create bare-room watermarks", async () => {
    clearDmCallActivities();
    const targetCall = archivedMucPm(
      "target-call",
      MUC_PM_ALICE,
      "",
      new Date().toISOString(),
    );
    targetCall.call_event = {
      kind: "propose",
      from: MUC_PM_ALICE,
      to: `${SELF}/web-test`,
      sid: "target-call",
      media: { audio: true, video: false },
    };
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => page([targetCall], { complete: true }),
    };
    const { pager, catchup } = createPager(xmpp);

    const result = await pager.queryPersonalMamPage(MUC_PM_ALICE);

    expect(result.messages).toEqual([]);
    expect(readDmCallActivity("room@muc.example.com")).toBeNull();
    expect(catchup.getDmLastSeen("room@muc.example.com")).toBeUndefined();
    clearDmCallActivities();
  });

  test("cold global personal-history call hydration ignores custom-service MUC-PM call events", async () => {
    clearDmCallActivities();
    const targetCall = archivedMucPm(
      "hydration-target-call",
      CUSTOM_MUC_PM_ALICE,
      "",
      new Date().toISOString(),
    );
    targetCall.call_event = {
      kind: "propose",
      from: CUSTOM_MUC_PM_ALICE,
      to: `${SELF}/web-test`,
      sid: "hydration-target-call",
      media: { audio: true, video: false },
    };
    const xmpp: MamWasmClient = {
      fetch_personal_history_page: async () => page([targetCall], { complete: true }),
    };
    const { pager } = createPager(xmpp, { isMucPmPeer: () => false });

    await pager.hydrateRecentDmCallActivities();

    expect(readDmCallActivity("room@rooms.waddle.example")).toBeNull();
    clearDmCallActivities();
  });

  test("global personal-history call hydration preserves ordinary bare DM calls", async () => {
    clearDmCallActivities();
    const ordinaryCall = archivedDm("ordinary-hydration-call", "", new Date().toISOString(), PEER);
    ordinaryCall.call_event = {
      kind: "propose",
      from: PEER,
      to: SELF,
      sid: "ordinary-hydration-call",
      media: { audio: true, video: false },
    };
    const xmpp: MamWasmClient = {
      fetch_personal_history_page: async () => page([ordinaryCall], { complete: true }),
    };
    const { pager } = createPager(xmpp, { isMucPmPeer: () => false });

    await pager.hydrateRecentDmCallActivities();

    expect(readDmCallActivity(PEER)?.sid).toBe("ordinary-hydration-call");
    clearDmCallActivities();
  });

  test("selected call hydration skips a peer with persisted MUC occupant scope", async () => {
    const catchup = new ReconnectCatchup();
    catchup.recordDmSeen(CUSTOM_MUC_PM_ALICE, new Date().toISOString(), undefined, [], "muc-occupant");
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([], { complete: true });
      },
    };
    const { pager } = createPager(xmpp, {
      catchup,
      isMucPmPeer: () => false,
    });

    await pager.hydrateRecentDmCallActivity(CUSTOM_MUC_PM_ALICE);

    expect(requestedPeers).toEqual([]);
  });

  test("latest pages keep the full requested occupant and reject sibling occupants before watermarking", async () => {
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([
          archivedMucPm("pm-bob", MUC_PM_BOB, "bob secret", "2026-07-01T10:00:00.000Z"),
          archivedMucPm("pm-alice", MUC_PM_ALICE, "alice secret", "2026-07-01T10:00:01.000Z"),
        ], { complete: false });
      },
    };
    const { pager, catchup } = createPager(xmpp);

    const result = await pager.queryPersonalMamPage(MUC_PM_ALICE, 50, { type: "latest" });

    expect(requestedPeers).toEqual([MUC_PM_ALICE]);
    expect(result.messages.map((message) => message.body)).toEqual(["alice secret"]);
    expect(result.firstArchiveId).toBe("pm-bob");
    expect(result.lastArchiveId).toBe("pm-alice");
    expect(result.complete).toBe(false);
    expect(catchup.getDmLastSeen(MUC_PM_ALICE)).toBe("2026-07-01T10:00:01.000Z");
    expect(catchup.getDmLastSeen(MUC_PM_BOB)).toBeUndefined();
  });

  test("before pages preserve their raw cursor while filtering sibling occupants", async () => {
    const calls: Array<{ peerJid: string; before?: string }> = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid, _max, pageParam) => {
        calls.push({
          peerJid,
          ...(pageParam.type === "before" ? { before: pageParam.before } : {}),
        });
        return page([
          archivedMucPm("older-bob", MUC_PM_BOB, "older bob", "2026-07-01T09:00:00.000Z"),
        ], { complete: false });
      },
    };
    const { pager } = createPager(xmpp);

    const result = await pager.queryPersonalMamPage(MUC_PM_ALICE, 50, {
      type: "before",
      before: "newer-boundary",
    });

    expect(calls).toEqual([{ peerJid: MUC_PM_ALICE, before: "newer-boundary" }]);
    expect(result.messages).toEqual([]);
    expect(result.firstArchiveId).toBe("older-bob");
    expect(result.lastArchiveId).toBe("older-bob");
    expect(result.complete).toBe(false);
  });

  test("thread pages keep the full requested occupant and reject sibling occupants", async () => {
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_by_thread: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([
          archivedMucPm("thread-bob", MUC_PM_BOB, "bob reply", "2026-07-01T10:00:00.000Z"),
          archivedMucPm("thread-alice", MUC_PM_ALICE, "alice reply", "2026-07-01T10:00:01.000Z"),
        ], { complete: true });
      },
    };
    const { pager, catchup } = createPager(xmpp);

    const result = await pager.queryPersonalMamThreadPage(MUC_PM_ALICE, "thread-1", 50);

    expect(requestedPeers).toEqual([MUC_PM_ALICE]);
    expect(result.messages.map((message) => message.body)).toEqual(["alice reply"]);
    expect(catchup.getDmLastSeen(MUC_PM_ALICE)).toBe("2026-07-01T10:00:01.000Z");
    expect(catchup.getDmLastSeen(MUC_PM_BOB)).toBeUndefined();
  });

  test("search keeps the full requested occupant and rejects sibling results", async () => {
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      search_dm_history: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([
          archivedMucPm("search-bob", MUC_PM_BOB, "matching bob", "2026-07-01T10:00:00.000Z"),
          archivedMucPm("search-alice", MUC_PM_ALICE, "matching alice", "2026-07-01T10:00:01.000Z"),
        ], { complete: true });
      },
    };
    const { pager } = createPager(xmpp);

    const result = await pager.searchDmMessages(MUC_PM_ALICE, "matching", 20);

    expect(requestedPeers).toEqual([MUC_PM_ALICE]);
    expect(result.map((message) => ({ body: message.body, peerJid: message.peerJid }))).toEqual([
      { body: "matching alice", peerJid: MUC_PM_ALICE },
    ]);
  });

  test("forward reconnect catch-up emits and watermarks only the requested occupant", async () => {
    const requestedPeers: string[] = [];
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async (peerJid) => {
        requestedPeers.push(peerJid);
        return page([
          archivedMucPm("catchup-bob", MUC_PM_BOB, "missed bob", "2026-07-01T10:00:00.000Z"),
          archivedMucPm("catchup-alice", MUC_PM_ALICE, "missed alice", "2026-07-01T10:00:01.000Z"),
        ], { complete: true });
      },
    };
    const { pager, events, catchup } = createPager(xmpp);
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));

    await pager.runReconnectCatchup(
      xmpp,
      [{ kind: "dm", key: MUC_PM_ALICE, scope: "muc-occupant", after: "before-gap" }],
      "fresh",
    );

    expect(requestedPeers).toEqual([MUC_PM_ALICE]);
    expect(delivered.map((message) => message.body)).toEqual(["missed alice"]);
    expect(catchup.getDmLastSeen(MUC_PM_ALICE)).toBe("2026-07-01T10:00:01.000Z");
    expect(catchup.getDmLastSeen(MUC_PM_BOB)).toBeUndefined();
  });

  test("timestamp reconnect fallback rejects sibling messages and all MUC-PM call-event side effects", async () => {
    clearDmCallActivities();
    const now = Date.now();
    const siblingTimestamp = new Date(now - 1_000).toISOString();
    const targetTimestamp = new Date(now).toISOString();
    const since = new Date(now - 2_000).toISOString();
    const siblingCall = archivedMucPm(
      "timestamp-bob-call",
      MUC_PM_BOB,
      "",
      siblingTimestamp,
    );
    siblingCall.call_event = {
      kind: "propose",
      from: MUC_PM_BOB,
      to: `${SELF}/web-test`,
      sid: "sibling-call",
      media: { audio: true, video: false },
    };
    const targetCall = archivedMucPm(
      "timestamp-alice-call",
      MUC_PM_ALICE,
      "",
      targetTimestamp,
    );
    targetCall.call_event = {
      kind: "propose",
      from: MUC_PM_ALICE,
      to: `${SELF}/web-test`,
      sid: "target-call",
      media: { audio: true, video: false },
    };
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => page([
        siblingCall,
        targetCall,
        archivedMucPm("timestamp-alice", MUC_PM_ALICE, "recent alice", targetTimestamp),
      ], { complete: true }),
    };
    const { pager, events, catchup } = createPager(xmpp);
    const delivered: LiveDmMessage[] = [];
    events.on("directMessage", (message) => delivered.push(message));

    await pager.runReconnectCatchup(
      xmpp,
      [{ kind: "dm", key: MUC_PM_ALICE, scope: "muc-occupant", since }],
      "fresh",
    );

    expect(delivered.map((message) => message.body)).toEqual(["recent alice"]);
    expect(catchup.getDmLastSeen(MUC_PM_BOB)).toBeUndefined();
    expect(readDmCallActivity("room@muc.example.com")).toBeNull();
    clearDmCallActivities();
  });

  test("catch-up re-emissions re-key MUC PMs by the occupant JID", async () => {
    const pmFromOccupant: WasmArchivedMessage = {
      mam_id: "pm-1",
      id: "msg-pm-1",
      from: "room@muc.example.com/juliet",
      to: SELF,
      body: "whispered",
      message_type: "chat",
      timestamp: "2026-07-01T10:00:01.000Z",
    };
    const xmpp: MamWasmClient = {
      fetch_dm_history_page: async () => page([pmFromOccupant], { complete: true }),
    };
    const { pager, events } = createPager(xmpp);
    const received: LiveDmMessage[] = [];
    events.set("directMessage", (message) => received.push(message));

    await pager.runReconnectCatchup(
      xmpp,
      [{ kind: "dm", key: "room@muc.example.com/juliet", scope: "muc-occupant", since: "2026-07-01T09:00:00.000Z" }],
      "fresh",
    );

    expect(received).toHaveLength(1);
    expect(received[0]).toMatchObject({
      peerJid: "room@muc.example.com/juliet",
      mucPm: true,
      nick: "juliet",
    });
  });
});

describe("rawMessageSeenIds stanza-id verification (#1267 item 2)", () => {
  test("keeps only stanza-ids stamped by a trusted archiving authority", () => {
    const ids = rawMessageSeenIds(
      {
        id: "wire-1",
        origin_id: "origin-1",
        stanza_id: "spoofed-sid",
        stanza_id_by: "mallory@evil.example",
        stanza_ids: [
          { id: "spoofed-sid-2", by: "mallory@evil.example" },
          { id: "server-sid", by: "example.com" },
          { id: "account-sid", by: SELF },
        ],
        message_type: "chat",
        reaction_emojis: [],
        markup_spans: [],
        mention_uris: [],
        references: [],
        is_muc: false,
        is_sticker: false,
        shared_files: [],
        link_previews: [],
        is_retracted: false,
        displayed_marker_requested: false,
      },
      [SELF, "example.com"],
    );

    // Sender-owned ids always count; XEP-0359 stanza-ids only when their
    // `by` matches an expected authority (§Security Considerations).
    expect(ids.sort()).toEqual(["account-sid", "origin-1", "server-sid", "wire-1"].sort());
    expect(ids).not.toContain("spoofed-sid");
    expect(ids).not.toContain("spoofed-sid-2");
  });

  test("accepts the singular stanza-id when its by matches the authority", () => {
    const ids = rawMessageSeenIds(
      {
        id: "wire-2",
        stanza_id: "room-sid",
        stanza_id_by: "room@muc.example.com",
        message_type: "groupchat",
        reaction_emojis: [],
        markup_spans: [],
        mention_uris: [],
        references: [],
        is_muc: true,
        is_sticker: false,
        shared_files: [],
        link_previews: [],
        is_retracted: false,
        displayed_marker_requested: false,
      },
      ["room@muc.example.com"],
    );
    expect(ids.sort()).toEqual(["room-sid", "wire-2"].sort());
  });
});

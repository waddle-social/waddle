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
import { MamPager, type MamWasmClient } from "../src/lib/xmpp/client-mam";
import { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";
import type { LiveDmMessage, XmppErrorEvent } from "../src/lib/xmpp/types";
import type { WasmArchivedMessage, WasmMamPage } from "../src/lib/xmpp/wasm-types";

const SELF = "alice@example.com";
const PEER = "bob@example.com";

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

function page(messages: WasmArchivedMessage[], opts: { complete?: boolean } = {}): WasmMamPage {
  return {
    messages,
    ...(messages.length > 0 ? { first_id: messages[0].mam_id, last_id: messages[messages.length - 1].mam_id } : {}),
    is_complete: opts.complete ?? false,
  };
}

function createPager(xmpp: MamWasmClient, overrides: { currentXmpp?: () => MamWasmClient | null } = {}) {
  const events = new TypedEventBus<ClientEvents>();
  const catchup = new ReconnectCatchup();
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
    ensureRoomReady: async () => undefined,
    roomJidForChannel: (channelId) => `${channelId}@muc.example.com`,
    isCurrentConnected: (candidate) =>
      overrides.currentXmpp ? overrides.currentXmpp() === candidate : xmpp === candidate,
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

    await pager.runReconnectCatchup(xmpp, [{ kind: "dm", key: PEER, after: "a0" }]);

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

    await pager.runReconnectCatchup(xmpp, [{ kind: "dm", key: PEER, after: "a0" }]);

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
      { kind: "dm", key: PEER, after: "gone", since: "2024-01-01T00:00:01Z" },
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

    await pager.runReconnectCatchup(xmpp, [{ kind: "dm", key: PEER, after: "a0" }]);

    expect(delivered).toEqual([]);
    expect(catchupInfos).toEqual([{ outcome: "aborted" }]);
  });
});

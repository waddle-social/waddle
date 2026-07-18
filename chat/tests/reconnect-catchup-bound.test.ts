/**
 * #1221 — bounded reconnect MAM catch-up.
 *
 * The reconnect catch-up paged each conversation (100 msgs/page) until
 * the archive was complete or the `since` boundary was crossed, with NO
 * page budget. A 4,425-message room archive was paged in full on every
 * fresh reconnect — the unbounded replay is what overflowed a victim's
 * SM queue during the prod join storm.
 *
 * Each of the four catch-up loops (dm/room × forward-cursor/timestamp)
 * now stops after RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION (5, ≤500
 * msgs) and throws into the existing per-conversation `catchupFailure`
 * fallback, which triggers a bounded wholesale reload; older history
 * lazy-loads via the normal pagination.
 *
 * These tests poke private state on `BrowserXmppClient` directly — see
 * the comment block in `resume-ordering.test.ts` for the rationale.
 */
import { afterEach, describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";
import type { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";

const MAX_PAGES = 5;

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

type PrivateState = {
  xmpp: unknown | null;
  connected: boolean;
  connectEpoch: number;
  catchup: ReconnectCatchup;
  outboundQueueHydration: Promise<void>;
  outboundQueue: {
    beginConnectionGeneration: (generation: number) => number;
    whenQuiescent: () => Promise<void>;
  };
  runSessionReady: (xmpp: unknown, lifecycle: { type: "fresh" | "resumed" }) => Promise<void>;
};

const createdClients: BrowserXmppClient[] = [];

function createClient(): BrowserXmppClient {
  const client = new BrowserXmppClient(session(), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
  });
  createdClients.push(client);
  return client;
}

async function makeClient() {
  const client = createClient();
  const state = client as unknown as PrivateState;
  await state.outboundQueueHydration;
  await state.outboundQueue.whenQuiescent();
  state.outboundQueue.beginConnectionGeneration(state.connectEpoch);
  const failures: Array<{ kind: "dm" | "room"; key: string }> = [];
  client.setCatchupFailureHandler((failure) => failures.push(failure));
  return { client, state, failures };
}

afterEach(async () => {
  const clients = createdClients.splice(0);
  for (const client of clients) {
    const state = client as unknown as PrivateState;
    state.xmpp = null;
    state.connected = false;
  }
  await Promise.all(clients.map((client) => client.dispose()));
});

/**
 * A forward (`after`-cursor) page source: never complete, advancing
 * `last_id`, empty messages — the loop would page forever without a
 * budget. Completes at page 50 as a safety net so a missing budget
 * fails fast on the call count instead of hanging.
 */
function forwardPages() {
  let calls = 0;
  return mock(async () => {
    calls += 1;
    return { messages: [], is_complete: calls >= 50, last_id: `cur-${calls}` };
  });
}

/** As `forwardPages` but for the backward timestamp loop: advancing
 *  `first_id`, no message crosses `since`. */
function backwardPages() {
  let calls = 0;
  return mock(async () => {
    calls += 1;
    return { messages: [], is_complete: calls >= 50, first_id: `first-${calls}` };
  });
}

describe("#1221 reconnect catch-up is bounded per conversation", () => {
  test("room forward-cursor catch-up stops at the page budget and fails over", async () => {
    const { state, failures } = await makeClient();
    const fetch_room_history_page = forwardPages();
    const xmpp = { fetch_room_history_page };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:00.000Z", "archive-0");

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(fetch_room_history_page).toHaveBeenCalledTimes(MAX_PAGES);
    expect(failures).toEqual([{ kind: "room", key: "c1@muc.example.com" }]);
  });

  test("room timestamp catch-up stops at the page budget and fails over", async () => {
    const { state, failures } = await makeClient();
    const fetch_room_history_page = backwardPages();
    const xmpp = { fetch_room_history_page };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    // No archive id → timestamp (backward) loop.
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:00.000Z");

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(fetch_room_history_page).toHaveBeenCalledTimes(MAX_PAGES);
    expect(failures).toEqual([{ kind: "room", key: "c1@muc.example.com" }]);
  });

  test("dm forward-cursor catch-up stops at the page budget and fails over", async () => {
    const { state, failures } = await makeClient();
    const fetch_dm_history_page = forwardPages();
    const xmpp = { fetch_dm_history_page };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordDmSeen("bob@example.com", "2026-07-01T10:00:00.000Z", "archive-0");

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(fetch_dm_history_page).toHaveBeenCalledTimes(MAX_PAGES);
    expect(failures).toEqual([{ kind: "dm", key: "bob@example.com" }]);
  });

  test("RESUMED page-budget exhaustion emits catchupFailure (gap affordance, #1267)", async () => {
    // A transient failure on a resumed stream stays silent (the stream
    // itself is gap-free), but budget exhaustion means messages beyond
    // the cap were genuinely not replayed — a real archive gap that the
    // wholesale-reload fallback must close.
    const { state, failures } = await makeClient();
    const fetch_room_history_page = forwardPages();
    const xmpp = { fetch_room_history_page };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:00.000Z", "archive-0");

    await state.runSessionReady(xmpp, { type: "resumed" });

    expect(fetch_room_history_page).toHaveBeenCalledTimes(MAX_PAGES);
    expect(failures).toEqual([{ kind: "room", key: "c1@muc.example.com" }]);
  });

  test("dm timestamp catch-up stops at the page budget and fails over", async () => {
    const { state, failures } = await makeClient();
    const fetch_dm_history_page = backwardPages();
    const xmpp = { fetch_dm_history_page };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordDmSeen("bob@example.com", "2026-07-01T10:00:00.000Z");

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(fetch_dm_history_page).toHaveBeenCalledTimes(MAX_PAGES);
    expect(failures).toEqual([{ kind: "dm", key: "bob@example.com" }]);
  });
});

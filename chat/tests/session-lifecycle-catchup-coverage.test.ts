/**
 * #1180 — fresh-reconnect double-MAM race.
 *
 * On a fresh session-ready the client runs its own reconnect catch-up
 * (cursor paging merged via live-merge). The `sessionLifecycle` event
 * it emits must carry which conversations that catch-up covers, so
 * timeline consumers can skip their wholesale MAM reload for a covered
 * conversation instead of racing the catch-up's merges.
 *
 * These tests poke private state on `BrowserXmppClient` directly — see
 * the comment block in `resume-ordering.test.ts` for the rationale.
 */
import { afterEach, describe, expect, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { XmppErrorEvent } from "../src/lib/xmpp/types";
import type { SessionLifecycleEvent } from "../src/lib/xmpp/types";
import type { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";

const createdClients = new Set<BrowserXmppClient>();

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

type PrivateState = {
  xmpp: unknown;
  connectEpoch: number;
  outboundQueue: { beginConnectionGeneration(generation: number): number };
  catchup: ReconnectCatchup;
  runSessionReady: (
    xmpp: unknown,
    lifecycle: { type: "resumed" | "fresh" },
  ) => Promise<void>;
};

function makeClient() {
  const client = new BrowserXmppClient(session(), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
  });
  createdClients.add(client);
  const state = client as unknown as PrivateState;
  state.outboundQueue.beginConnectionGeneration(0);
  const xmpp = {};
  state.xmpp = xmpp;
  const received: SessionLifecycleEvent[] = [];
  client.setSessionLifecycleHandler((event) => received.push(event));
  return { client, state, xmpp, received };
}

afterEach(async () => {
  const clients = [...createdClients];
  createdClients.clear();
  await Promise.all(clients.map((client) => {
    const state = client as unknown as PrivateState;
    state.xmpp = null;
    state.connectEpoch = 0;
    return client.dispose();
  }));
});

describe("#1180 fresh lifecycle carries reconnect catch-up coverage", () => {
  test("fresh session-ready reports the conversations the catch-up will page", async () => {
    const { state, xmpp, received } = makeClient();
    // Arm the tracker: the first session-started is initial login
    // (nothing to catch up on); cursors recorded before the *next*
    // session-started are what the catch-up covers.
    state.catchup.onSessionStarted();
    state.catchup.recordDmSeen("bob@example.com", "2026-07-01T10:00:00.000Z");
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:01.000Z");

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(received).toEqual([
      {
        type: "fresh",
        catchup: {
          dmJids: ["bob@example.com"],
          roomJids: ["c1@muc.example.com"],
        },
      },
    ]);
  });

  test("fresh session-ready with nothing to catch up on reports empty coverage", async () => {
    const { state, xmpp, received } = makeClient();

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(received).toEqual([
      { type: "fresh", catchup: { dmJids: [], roomJids: [] } },
    ]);
  });

  test("a failed per-conversation catch-up emits catchupFailure with the entry key", async () => {
    const { client, state, received } = makeClient();
    const xmpp = {
      fetch_room_history_page: () => {
        throw new Error("boom");
      },
    };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:01.000Z");
    const failures: Array<{ kind: "dm" | "room"; key: string }> = [];
    client.setCatchupFailureHandler((failure) => failures.push(failure));

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(failures).toEqual([{ kind: "room", key: "c1@muc.example.com" }]);
    // The lifecycle event still reported coverage — the failure signal
    // is the consumer's cue to run the reload it skipped.
    expect(received).toEqual([
      { type: "fresh", catchup: { dmJids: [], roomJids: ["c1@muc.example.com"] } },
    ]);
  });

  test("an entry aborted by disconnect emits no catchupFailure", async () => {
    // The fallback reload must never fire against a dead handle: when
    // the connection dropped mid-catch-up, the NEXT session-ready owns
    // recovery (its coverage/catch-up run starts over).
    const { client, state, received } = makeClient();
    const xmpp = {
      fetch_room_history_page: () => {
        // Simulate the connection dropping mid-fetch: a new handle
        // takes over before the error propagates.
        state.xmpp = { tag: "successor" };
        throw new Error("socket closed");
      },
    };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:01.000Z");
    const failures: Array<{ kind: "dm" | "room"; key: string }> = [];
    client.setCatchupFailureHandler((failure) => failures.push(failure));

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(failures).toEqual([]);
    expect(received).toEqual([
      { type: "fresh", catchup: { dmJids: [], roomJids: ["c1@muc.example.com"] } },
    ]);
  });

  test("a successful catch-up emits no catchupFailure", async () => {
    const { client, state, received } = makeClient();
    const xmpp = {
      fetch_room_history_page: async () => ({ messages: [], complete: true }),
    };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:01.000Z");
    const failures: Array<{ kind: "dm" | "room"; key: string }> = [];
    client.setCatchupFailureHandler((failure) => failures.push(failure));

    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(failures).toEqual([]);
    expect(received).toEqual([
      { type: "fresh", catchup: { dmJids: [], roomJids: ["c1@muc.example.com"] } },
    ]);
  });

  test("a throwing catchupFailure handler surfaces as an error event, not an unhandled rejection", async () => {
    // The fallback handler is business-critical: if it throws, the
    // failure must land on the typed error channel (visible, correctly
    // attributed) while the session-ready flow still completes.
    const { client, state } = makeClient();
    const xmpp = {
      fetch_room_history_page: () => {
        throw new Error("boom");
      },
    };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:01.000Z");
    client.setCatchupFailureHandler(() => {
      throw new Error("handler broke");
    });
    const errors: XmppErrorEvent[] = [];
    client.onError((event) => errors.push(event));

    // Must resolve — a throwing handler must not reject the barrier.
    await state.runSessionReady(xmpp, { type: "fresh" });

    expect(
      errors.some((event) => (
        event.kind === "history"
        && event.detail.includes("catch-up fallback handler failed")
      )),
    ).toBe(true);
  });

  test("a failed catch-up on a RESUMED session emits no catchupFailure", async () => {
    // Resumed sessions never skipped a reload (the fresh-only lifecycle
    // path is the only skipper), so there is nothing to fall back to —
    // emitting here would trigger a spurious wholesale reload for a
    // gap-free stream.
    const { client, state, received } = makeClient();
    const xmpp = {
      fetch_room_history_page: () => {
        throw new Error("boom");
      },
    };
    state.xmpp = xmpp;
    state.catchup.onSessionStarted();
    state.catchup.recordRoomSeen("c1@muc.example.com", "2026-07-01T10:00:01.000Z");
    const failures: Array<{ kind: "dm" | "room"; key: string }> = [];
    client.setCatchupFailureHandler((failure) => failures.push(failure));

    await state.runSessionReady(xmpp, { type: "resumed" });

    expect(failures).toEqual([]);
    expect(received).toEqual([{ type: "resumed" }]);
  });

  test("resumed session-ready carries no coverage field", async () => {
    const { state, xmpp, received } = makeClient();
    state.catchup.onSessionStarted();
    state.catchup.recordDmSeen("bob@example.com", "2026-07-01T10:00:00.000Z");

    await state.runSessionReady(xmpp, { type: "resumed" });

    expect(received).toEqual([{ type: "resumed" }]);
  });
});

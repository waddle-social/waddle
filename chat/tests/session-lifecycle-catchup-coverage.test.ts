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
import { describe, expect, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { SessionLifecycleEvent } from "../src/lib/xmpp/types";
import type { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";

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
  catchup: ReconnectCatchup;
  runSessionReady: (
    xmpp: unknown,
    lifecycle: { type: "resumed" | "fresh" },
  ) => Promise<void>;
};

function makeClient() {
  const client = new BrowserXmppClient(session());
  const state = client as unknown as PrivateState;
  const xmpp = {};
  state.xmpp = xmpp;
  const received: SessionLifecycleEvent[] = [];
  client.setSessionLifecycleHandler((event) => received.push(event));
  return { client, state, xmpp, received };
}

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

  test("resumed session-ready carries no coverage field", async () => {
    const { state, xmpp, received } = makeClient();
    state.catchup.onSessionStarted();
    state.catchup.recordDmSeen("bob@example.com", "2026-07-01T10:00:00.000Z");

    await state.runSessionReady(xmpp, { type: "resumed" });

    expect(received).toEqual([{ type: "resumed" }]);
  });
});

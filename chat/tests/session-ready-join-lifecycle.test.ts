/**
 * #1221 — session-ready MUC-join lifecycle.
 *
 * The auto-join fan-out sat OUTSIDE the `fresh` branch, so it re-sent a
 * full join presence for every room on every `resumed` session-ready too
 * — even though MUC occupancy survives an SM detach-for-resume
 * server-side (no leave is broadcast; see `cleanup.rs`). This drove the
 * reconnect join storm.
 *
 * The fix:
 *   * fan-out only on `fresh`;
 *   * on disconnect, snapshot the self-presence-confirmed (XEP-0045
 *     status 110) room keys into `resumedSessionRoomKeys`;
 *   * on `resumed`, re-seed the join trackers from that snapshot WITHOUT
 *     sending presence, so `roomIsReady` and the queued-send flush keep
 *     working.
 *
 * These tests poke private state on `BrowserXmppClient` directly — see
 * the comment block in `resume-ordering.test.ts` for the rationale.
 */
import { describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { SessionLifecycleEvent } from "../src/lib/xmpp/types";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

type PresenceCb = (presence: {
  from?: string;
  presence_type?: string;
  muc_jid?: string;
}) => void;

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  destroying: boolean;
  wireEvents: (xmpp: unknown) => void;
  runSessionReady: (xmpp: unknown, lifecycle: { type: "fresh" | "resumed" }) => Promise<void>;
  handleDisconnected: (xmpp: unknown, error?: Error) => void;
  joinedMucReady: Set<string>;
  joinedMucs: Map<string, Promise<void>>;
  retainedJoinedRoomJids: Set<string>;
  resumedSessionRoomKeys: Set<string>;
  autoJoinAttemptedRoomKeys: Set<string>;
  fullJid: string;
};

function connectedClient() {
  const client = new BrowserXmppClient(session());
  const state = client as unknown as PrivateState;
  let onPresence: PresenceCb | null = null;
  const joinRoom = mock(async () => undefined);
  const xmpp = {
    join_room: joinRoom,
    set_on_presence(cb: PresenceCb) {
      onPresence = cb;
    },
  };
  state.xmpp = xmpp;
  state.connected = true;
  state.wireEvents(xmpp);
  const received: SessionLifecycleEvent[] = [];
  client.setSessionLifecycleHandler((event) => received.push(event));
  const deliverSelfPresence = (roomBareJid: string) => {
    onPresence?.({
      from: `${roomBareJid}/alice`,
      presence_type: "available",
      muc_jid: state.fullJid,
    });
  };
  return { client, state, xmpp, joinRoom, received, deliverSelfPresence };
}

describe("#1221 resumed session-ready does not rejoin", () => {
  test("restores readiness from the snapshot and sends no join presence", async () => {
    const { state, xmpp, joinRoom } = connectedClient();
    const room = "c1@conference.example.com";
    // Retained rooms would normally be fanned out — prove they are NOT
    // on resumed. The snapshot is the only re-seed source.
    state.retainedJoinedRoomJids = new Set([room]);
    state.resumedSessionRoomKeys = new Set([room]);

    await state.runSessionReady(xmpp, { type: "resumed" });

    expect(joinRoom).not.toHaveBeenCalled();
    expect(state.joinedMucReady.has(room)).toBe(true);
    expect(state.joinedMucs.has(room)).toBe(true);
  });

  test("disconnect snapshots the self-presence-confirmed rooms", () => {
    const { state, xmpp } = connectedClient();
    const room = "c1@conference.example.com";
    state.joinedMucReady.add(room);
    state.joinedMucs.set(room, Promise.resolve());
    // `destroying` makes handleDisconnected return after the snapshot +
    // tracker clears but before it schedules a reconnect timer.
    state.destroying = true;

    state.handleDisconnected(xmpp);

    expect([...state.resumedSessionRoomKeys]).toEqual([room]);
  });

  test("a resume after a real join sends no new join presence (full path)", async () => {
    const { state, xmpp, joinRoom, deliverSelfPresence } = connectedClient();
    const room = "c3@conference.example.com";
    // Real join populates joinedMucReady via self-presence.
    const joined = state.ensureJoined(room);
    deliverSelfPresence(room);
    await joined;
    expect(joinRoom).toHaveBeenCalledTimes(1);

    // Transient disconnect captures the snapshot (destroying skips the timer).
    state.destroying = true;
    state.handleDisconnected(xmpp);
    state.destroying = false;

    // Reconnect with a fresh handle and resume.
    const joinRoom2 = mock(async () => undefined);
    const xmpp2 = { join_room: joinRoom2, set_on_presence() {} };
    state.xmpp = xmpp2;
    state.connected = true;
    state.wireEvents(xmpp2);

    await state.runSessionReady(xmpp2, { type: "resumed" });

    expect(joinRoom2).not.toHaveBeenCalled();
    expect(state.joinedMucReady.has(room)).toBe(true);
  });

  test("a resume whose snapshot was lost (page reload) rejoins retained rooms", async () => {
    // The pagehide handoff persists the SM resume state but not the
    // in-memory readiness snapshot, so a reloaded client resumes with an
    // empty resumedSessionRoomKeys. It must fall back to rejoining the
    // retained set (single-flight) rather than leaving rooms un-ready.
    const { state, xmpp, joinRoom, deliverSelfPresence } = connectedClient();
    const room = "c7@conference.example.com";
    state.retainedJoinedRoomJids = new Set([room]);
    // resumedSessionRoomKeys intentionally left empty (snapshot lost).

    const ready = state.runSessionReady(xmpp, { type: "resumed" });
    deliverSelfPresence(room);
    await ready;
    await Promise.resolve();

    expect(joinRoom).toHaveBeenCalledTimes(1);
    expect(joinRoom).toHaveBeenCalledWith(room, "alice");
  });
});

describe("#1221 session-ready runs once per handle", () => {
  test("a duplicate no-catchup session-ready for the same handle is a no-op", async () => {
    // Three event hooks call handleSessionReady on the same handle. The
    // resumeBarrier gate only covered the catch-up path; the no-catch-up
    // branch returned without opening a barrier, so a second callback
    // re-admitted and double-emitted the lifecycle + double-ran setup.
    const { state, xmpp, joinRoom, received, deliverSelfPresence } = connectedClient();
    const room = "c5@conference.example.com";
    state.retainedJoinedRoomJids = new Set([room]);

    const first = state.runSessionReady(xmpp, { type: "fresh" });
    deliverSelfPresence(room);
    await first;

    await state.runSessionReady(xmpp, { type: "fresh" }); // duplicate hook

    expect(received.filter((event) => event.type === "fresh")).toHaveLength(1);
    expect(joinRoom).toHaveBeenCalledTimes(1);
  });
});

describe("#1221 fresh session-ready still rejoins", () => {
  test("rejoins each retained room exactly once", async () => {
    const { state, xmpp, joinRoom, deliverSelfPresence } = connectedClient();
    const room = "c2@conference.example.com";
    state.retainedJoinedRoomJids = new Set([room]);

    const ready = state.runSessionReady(xmpp, { type: "fresh" });
    deliverSelfPresence(room);
    await ready;
    await Promise.resolve();

    expect(joinRoom).toHaveBeenCalledTimes(1);
    expect(joinRoom).toHaveBeenCalledWith(room, "alice");
  });
});

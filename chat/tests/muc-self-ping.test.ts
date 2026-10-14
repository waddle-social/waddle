/**
 * XEP-0410 MUC self-ping: ping the room-assigned own nick and rejoin only
 * on errors that §3.2 says mean "no longer joined".
 */
import { afterEach, describe, expect, test } from "bun:test";
import { $callState } from "../src/lib/calls/call-store";
import { RoomJoinRetryCancelledError } from "../src/lib/xmpp/room-join-retry";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  currentRoom: string | null;
  joinedMucReady: Set<string>;
  events: { emitSafe: (name: "ownOccupantNick", roomJid: string, nick: string | null) => void };
  ensureJoined: (roomJid: string) => Promise<void>;
  doSelfPing: () => Promise<void>;
  catchup: {
    recordRoomSeen: (roomJid: string, at: string, archiveId?: string) => void;
    onSessionStarted: () => unknown[];
  };
  runReconnectCatchup: (xmpp: unknown, entries: unknown[], lifecycle: string) => Promise<void>;
  presence: { memberJidsFor: (roomJid: string) => Record<string, string> };
  selfPingTimer: unknown;
  startSelfPing: () => void;
  stopSelfPing: () => void;
  handleDisconnected: (xmpp: unknown, error?: Error) => void;
  reconnect: { clearTimer: () => void };
  readvertiseActiveMucCallPresence: (xmpp: unknown) => Promise<void>;
  runSessionReady: (xmpp: unknown, lifecycle: { type: "fresh" | "resumed" }) => Promise<void>;
  flushQueuedRoomMessages: (roomJid: string) => Promise<void>;
  roomIsTerminallyDenied: (roomJid: string) => boolean;
};

afterEach(() => {
  $callState.set({ phase: "idle" });
});

const ROOM = "general@muc.example.com";

function harness(reject?: { condition: string }) {
  const client = new BrowserXmppClient({
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession);
  const state = client as unknown as PrivateState;
  const sent: string[] = [];
  state.xmpp = {
    send_raw_iq: async (xml: string) => {
      sent.push(xml);
      if (reject) throw Object.assign(new Error(reject.condition), reject);
      return "<iq type='result'/>";
    },
  };
  state.connected = true;
  state.currentRoom = ROOM;
  state.joinedMucReady.add(ROOM);
  const rejoins: string[] = [];
  state.ensureJoined = async (roomJid) => {
    rejoins.push(roomJid);
    state.joinedMucReady.add(roomJid);
  };
  const catchups: Array<{ entries: unknown[]; lifecycle: string }> = [];
  state.runReconnectCatchup = async (_xmpp, entries, lifecycle) => { catchups.push({ entries, lifecycle }); };
  const failures: Array<{ kind: string; key: string }> = [];
  client.setCatchupFailureHandler((failure) => failures.push(failure));
  return { client, state, sent, rejoins, catchups, failures };
}

describe("XEP-0410 MUC self-ping", () => {
  test("pings the nick the room assigned, escaped", async () => {
    const { state, sent, rejoins } = harness();
    state.events.emitSafe("ownOccupantNick", ROOM, "alice's \"laptop\"");

    await state.doSelfPing();

    expect(sent).toHaveLength(1);
    expect(sent[0]).toContain(`to="${ROOM}/alice&apos;s &quot;laptop&quot;"`);
    expect(rejoins).toEqual([]);
  });

  test("rejoins when the room says we are not an occupant, then pages in the missed stretch", async () => {
    const { state, rejoins, catchups } = harness({ condition: "not-acceptable" });
    state.catchup.recordRoomSeen(ROOM, "2024-01-01T00:00:00.000Z", "arch-1");

    await state.doSelfPing();
    await Promise.resolve();

    // Readiness was revoked and the rejoin restored it.
    expect(rejoins).toEqual([ROOM]);
    expect(state.joinedMucReady.has(ROOM)).toBe(true);
    expect(catchups).toEqual([{
      entries: [{ kind: "room", key: ROOM, after: "arch-1", since: "2024-01-01T00:00:00.000Z" }],
      lifecycle: "fresh",
    }]);
  });

  test("without a catch-up cursor the rejoined room signals catch-up failure for the timeline to reload", async () => {
    const { state, catchups, failures } = harness({ condition: "not-acceptable" });

    await state.doSelfPing();
    await Promise.resolve();

    expect(catchups).toEqual([]);
    expect(failures).toEqual([{ kind: "room", key: ROOM }]);
  });

  test("a rejoin forgets occupants seen before the room dropped us and tells the UI", async () => {
    const { client, state } = harness({ condition: "not-acceptable" });
    state.presence.memberJidsFor(ROOM).bob = "bob@example.com";
    const published: Array<Record<string, unknown>> = [];
    client.setPresenceHandler((presence) => published.push(presence));

    await state.doSelfPing();

    expect(state.presence.memberJidsFor(ROOM)).toEqual({});
    expect(published.at(-1)).toEqual({});
  });

  test("keeps the ping timer across a dropped connection and pings only once the room is ready again", async () => {
    const { state, sent } = harness();
    const handle = state.xmpp;
    state.startSelfPing();

    state.handleDisconnected(handle, new Error("socket closed"));
    state.reconnect.clearTimer();
    expect(state.selfPingTimer).not.toBeNull();
    await state.doSelfPing();
    expect(sent).toEqual([]);

    // Reconnected and rejoined: the surviving timer's next tick pings again.
    state.xmpp = handle;
    state.connected = true;
    state.joinedMucReady.add(ROOM);
    await state.doSelfPing();
    expect(sent).toHaveLength(1);
    state.stopSelfPing();
  });

  test("a rejoin that fails with no retry scheduled hands the room to the timeline's reload", async () => {
    const { state, failures, catchups } = harness({ condition: "not-acceptable" });
    state.ensureJoined = async () => { throw new Error("self-presence timeout"); };

    await state.doSelfPing();
    for (let i = 0; i < 5; i += 1) await Promise.resolve();

    expect(catchups).toEqual([]);
    expect(failures).toEqual([{ kind: "room", key: ROOM }]);
  });

  test("a rejoin that succeeds on a scheduled retry pages the gap without a reload", async () => {
    const { state, failures, catchups } = harness({ condition: "not-acceptable" });
    state.catchup.recordRoomSeen(ROOM, "2024-01-01T00:00:00.000Z", "arch-1");
    let retry: Promise<void> | undefined;
    let attempts = 0;
    (state as unknown as { roomJoinRetry: { pending: () => Promise<void> | undefined } }).roomJoinRetry.pending =
      () => retry;
    state.ensureJoined = async () => {
      attempts += 1;
      if (attempts === 1) {
        // The timed-out join schedules its next window before rejecting.
        retry = Promise.resolve();
        throw new Error("self-presence timeout");
      }
      retry = undefined;
      state.joinedMucReady.add(ROOM);
    };

    await state.doSelfPing();
    for (let i = 0; i < 5; i += 1) await Promise.resolve();

    expect(attempts).toBe(2);
    expect(failures).toEqual([]);
    expect(catchups).toHaveLength(1);
  });

  test("a rejoin during this room's call triggers the call re-advertisement", async () => {
    const { state } = harness({ condition: "not-acceptable" });
    const readvertised: unknown[] = [];
    state.readvertiseActiveMucCallPresence = async (xmpp) => { readvertised.push(xmpp); };
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: ROOM,
      sid: "muc-call",
      media: { audio: true, video: false },
      join: { url: "wss://livekit.example.test", room: "general", identity: "alice", token: "token" },
    });

    await state.doSelfPing();

    expect(readvertised).toEqual([state.xmpp]);
  });

  test("a cancelled join retry window does not end the active call", async () => {
    const { state } = harness();
    const advertised: string[] = [];
    Object.assign(state.xmpp as object, {
      update_muji_presence: async (roomJid: string) => { advertised.push(roomJid); },
    });
    let ended = 0;
    (state as unknown as { endCallAndClearProjections: () => Promise<void> }).endCallAndClearProjections =
      async () => { ended += 1; };
    let joins = 0;
    state.ensureJoined = async () => {
      joins += 1;
      if (joins <= 2) throw new RoomJoinRetryCancelledError();
    };
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: ROOM,
      sid: "muc-call",
      selfNick: "alice",
      media: { audio: true, video: false },
      join: { url: "wss://livekit.example.test", room: "general", identity: "alice", token: "token" },
    });

    await state.readvertiseActiveMucCallPresence(state.xmpp);

    expect(joins).toBe(3);
    expect(ended).toBe(0);
    expect(advertised).toEqual([ROOM]);
  });

  test("a rejoin flushes sends that queued while the room was not ready", async () => {
    const { state } = harness({ condition: "not-acceptable" });
    const flushed: string[] = [];
    state.flushQueuedRoomMessages = async (roomJid) => { flushed.push(roomJid); };

    await state.doSelfPing();
    for (let i = 0; i < 3; i += 1) await Promise.resolve();

    expect(flushed).toEqual([ROOM]);
  });

  test("a gap owed by a rejoin the drop interrupted is paged on the resumed session", async () => {
    const { state, catchups } = harness({ condition: "not-acceptable" });
    state.catchup.onSessionStarted(); // the initial login
    state.catchup.recordRoomSeen(ROOM, "2024-01-01T00:00:00.000Z", "arch-1");
    state.catchup.recordRoomSeen("other@muc.example.com", "2024-01-01T00:00:00.000Z", "arch-2");
    const resumed = { tag: "resumed" };
    state.ensureJoined = async () => {
      // The socket drops mid-rejoin; the reconnect resumes the session.
      state.xmpp = resumed;
      throw new Error("socket closed");
    };

    await state.doSelfPing();
    for (let i = 0; i < 3; i += 1) await Promise.resolve();
    expect(catchups).toEqual([]);

    await state.runSessionReady(resumed, { type: "resumed" });

    expect(catchups).toEqual([{
      entries: [{ kind: "room", key: ROOM, after: "arch-1", since: "2024-01-01T00:00:00.000Z" }],
      lifecycle: "fresh",
    }]);
  });

  test("a terminal denial during the call room's rejoin is not retried as a cancellation and ends the call", async () => {
    const { state } = harness();
    let ended = 0;
    (state as unknown as { endCallAndClearProjections: () => Promise<void> }).endCallAndClearProjections =
      async () => { ended += 1; };
    let joins = 0;
    state.ensureJoined = async () => {
      joins += 1;
      throw new RoomJoinRetryCancelledError();
    };
    state.roomIsTerminallyDenied = () => true;
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: ROOM,
      sid: "muc-call",
      selfNick: "alice",
      media: { audio: true, video: false },
      join: { url: "wss://livekit.example.test", room: "general", identity: "alice", token: "token" },
    });

    await state.readvertiseActiveMucCallPresence(state.xmpp);

    expect(joins).toBe(2);
    expect(ended).toBe(1);
  });

  test("a resume paging owed rooms does not leave a full catch-up owed when it is cut short", async () => {
    const { state, catchups } = harness({ condition: "not-acceptable" });
    state.catchup.onSessionStarted(); // the initial login
    state.catchup.recordRoomSeen(ROOM, "2024-01-01T00:00:00.000Z", "arch-1");
    state.catchup.recordRoomSeen("other@muc.example.com", "2024-01-01T00:00:00.000Z", "arch-2");
    const firstResume = { tag: "first" };
    const secondResume = { tag: "second" };
    state.ensureJoined = async () => {
      state.xmpp = firstResume;
      throw new Error("socket closed");
    };
    await state.doSelfPing();
    for (let i = 0; i < 3; i += 1) await Promise.resolve();
    // The owed-room catch-up on the first resume is cut short by another drop.
    state.runReconnectCatchup = async (_xmpp, entries, lifecycle) => {
      catchups.push({ entries, lifecycle });
      state.xmpp = secondResume;
    };

    await state.runSessionReady(firstResume, { type: "resumed" });
    await state.runSessionReady(secondResume, { type: "resumed" });

    const pagedRooms = catchups.map((run) => (run.entries as Array<{ key: string }>).map((entry) => entry.key));
    expect(pagedRooms).toEqual([[ROOM], [ROOM]]);
  });

  test("a gap found during a catch-up barrier stays owed after that barrier completes", async () => {
    const { state } = harness({ condition: "not-acceptable" });
    const owed = (state as unknown as { rejoinGapOwedRooms: Map<string, symbol> }).rejoinGapOwedRooms;
    state.catchup.onSessionStarted(); // the initial login
    state.catchup.recordRoomSeen("other@muc.example.com", "2024-01-01T00:00:00.000Z", "arch-2");
    state.ensureJoined = () => new Promise<void>(() => undefined); // rejoin still waiting
    state.runReconnectCatchup = async () => {
      // The self-ping fires while the barrier pages.
      await state.doSelfPing();
    };

    await state.runSessionReady(state.xmpp, { type: "fresh" });

    expect([...owed.keys()]).toEqual([ROOM]);
  });

  test("settling an older gap does not clear a newer one for the same room", async () => {
    const { state } = harness({ condition: "not-acceptable" });
    const owed = (state as unknown as { rejoinGapOwedRooms: Map<string, symbol> }).rejoinGapOwedRooms;
    state.catchup.onSessionStarted(); // the initial login
    state.catchup.recordRoomSeen(ROOM, "2024-01-01T00:00:00.000Z", "arch-1");
    const resumed = {
      send_raw_iq: async () => {
        throw Object.assign(new Error("not-acceptable"), { condition: "not-acceptable" });
      },
    };
    // First gap: the drop interrupts the rejoin, so it stays owed.
    state.ensureJoined = async () => {
      state.xmpp = resumed;
      throw new Error("socket closed");
    };
    await state.doSelfPing();
    for (let i = 0; i < 3; i += 1) await Promise.resolve();
    state.connected = true;
    state.joinedMucReady.add(ROOM);
    // The resume pages it, and a second self-ping finds the room gone again.
    state.ensureJoined = () => new Promise<void>(() => undefined);
    state.runReconnectCatchup = async () => {
      await state.doSelfPing();
    };

    await state.runSessionReady(resumed, { type: "resumed" });

    expect([...owed.keys()]).toEqual([ROOM]);
  });

  test.each(["service-unavailable", "feature-not-implemented", "item-not-found", "remote-server-timeout"])(
    "stays joined on %s",
    async (condition) => {
      const { state, rejoins } = harness({ condition });

      await state.doSelfPing();

      expect(state.joinedMucReady.has(ROOM)).toBe(true);
      expect(rejoins).toEqual([]);
    },
  );
});

/**
 * Joins of rooms on other servers through `BrowserXmppClient`: a failed
 * join carries the host's stanza error, leaving uses the nickname the room
 * assigned, and the room's reason for ending the occupancy reaches
 * listeners. Pokes private state like `muc-join-dedup.test.ts`.
 */
import { describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import { RoomJoinError } from "../src/lib/xmpp/room-join-error";
import { classifyRemoteError } from "../src/lib/xmpp/remote-outcome";

const REMOTE_ROOM = "dev@chat.remote.test";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

type Presence = {
  from?: string;
  presence_type?: string;
  muc_jid?: string;
  muc_status_codes?: number[];
  error_type?: string;
  error_condition?: string;
};

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  wireEvents: (xmpp: unknown) => void;
  retainedJoinedRoomJids: Set<string>;
  fullJid: string;
};

function connectedClient() {
  const client = new BrowserXmppClient(session());
  const state = client as unknown as PrivateState;
  let onPresence: ((presence: Presence) => void) | null = null;
  const joinRoom = mock(async () => undefined);
  const leaveRoom = mock(async () => undefined);
  const xmpp = {
    join_room: joinRoom,
    leave_room: leaveRoom,
    set_on_presence(cb: (presence: Presence) => void) {
      onPresence = cb;
    },
  };
  state.xmpp = xmpp;
  state.connected = true;
  state.wireEvents(xmpp);
  return {
    client,
    state,
    joinRoom,
    leaveRoom,
    deliver: (presence: Presence) => onPresence?.(presence),
  };
}

describe("rooms on other servers", () => {
  test("a room off the home MUC service is remote", () => {
    const { client } = connectedClient();
    expect(client.isRemoteRoom("general@muc.example.com")).toBe(false);
    expect(client.isRemoteRoom(REMOTE_ROOM)).toBe(true);
  });

  test("a join the router answers with an outage rejects with that condition", async () => {
    const { client, deliver } = connectedClient();
    const join = client.ensureJoined(REMOTE_ROOM);
    await Promise.resolve();
    deliver({
      from: `${REMOTE_ROOM}/alice`,
      presence_type: "error",
      error_type: "wait",
      error_condition: "remote-server-timeout",
    });
    const error = await join.catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(RoomJoinError);
    expect(error).toMatchObject({ condition: "remote-server-timeout", errorType: "wait", timedOut: false });
    expect(classifyRemoteError(error)).toMatchObject({ kind: "outage", retry: "automatic" });
  });

  test("a room that does not exist, or is closed to other servers, is a deletion", async () => {
    const { client, deliver } = connectedClient();
    const join = client.ensureJoined(REMOTE_ROOM);
    await Promise.resolve();
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "error", error_type: "cancel", error_condition: "item-not-found" });
    expect(classifyRemoteError(await join.catch((caught: unknown) => caught)).kind).toBe("deletion");
  });

  test("an outage, a denial and a missing room give three different messages", async () => {
    const messages: string[] = [];
    for (const [condition, errorType] of [
      ["remote-server-timeout", "wait"],
      ["registration-required", "auth"],
      ["item-not-found", "cancel"],
    ] as const) {
      const { client, deliver } = connectedClient();
      const join = client.ensureJoined(REMOTE_ROOM);
      await Promise.resolve();
      deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "error", error_type: errorType, error_condition: condition });
      const error = await join.catch((caught: unknown) => caught);
      messages.push((error as Error).message);
    }
    expect(new Set(messages).size).toBe(3);
    expect(messages[2]).toContain("not open to accounts on other servers");
  });

  test("a failed join is reported to listeners, whoever started it", async () => {
    const { client, deliver } = connectedClient();
    const failures: Array<[string, string | undefined]> = [];
    client.onRoomJoinFailed((roomJid, error) => failures.push([roomJid, error.condition]));
    const join = client.ensureJoined(REMOTE_ROOM);
    await Promise.resolve();
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "error", error_type: "cancel", error_condition: "item-not-found" });
    await join.catch(() => undefined);
    expect(failures).toEqual([[REMOTE_ROOM, "item-not-found"]]);
  });

  test("a home room keeps its existing join messages", async () => {
    const { client, deliver } = connectedClient();
    const join = client.ensureJoined("general@muc.example.com");
    await Promise.resolve();
    deliver({ from: "general@muc.example.com/alice", presence_type: "error", error_type: "cancel", error_condition: "item-not-found" });
    await expect(join).rejects.toThrow("Channel presence was rejected. Try again in a moment.");
  });

  test("leaving uses the room's nickname and stops rejoining after reconnects", async () => {
    const { client, state, leaveRoom, deliver } = connectedClient();
    const join = client.ensureJoined(REMOTE_ROOM);
    await Promise.resolve();
    deliver({ from: `${REMOTE_ROOM}/alice-2`, presence_type: "available", muc_status_codes: [110, 210] });
    await join;
    expect(state.retainedJoinedRoomJids.has(REMOTE_ROOM)).toBe(true);

    await client.leaveRoom(REMOTE_ROOM);
    expect(leaveRoom).toHaveBeenCalledWith(REMOTE_ROOM, "alice-2");
    expect(state.retainedJoinedRoomJids.has(REMOTE_ROOM)).toBe(false);
  });

  test("a ban ends rejoining in this session and after reconnects", async () => {
    const { client, state, deliver } = connectedClient();
    const join = client.ensureJoined(REMOTE_ROOM);
    await Promise.resolve();
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "available", muc_status_codes: [110] });
    await join;
    expect(state.retainedJoinedRoomJids.has(REMOTE_ROOM)).toBe(true);
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "unavailable", muc_status_codes: [110, 301] });
    expect(state.retainedJoinedRoomJids.has(REMOTE_ROOM)).toBe(false);
  });

  test("background joins keep the order their caller chose", async () => {
    const { client, joinRoom, deliver } = connectedClient();
    const fanOut = client.fanOutAutoJoin([REMOTE_ROOM, "general@muc.example.com"], 1);
    await Promise.resolve();
    await Promise.resolve();
    expect((joinRoom.mock.calls[0] as unknown[])[0]).toBe(REMOTE_ROOM);
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "available", muc_status_codes: [110] });
    await new Promise((resolve) => setTimeout(resolve, 0));
    deliver({ from: "general@muc.example.com/alice", presence_type: "available", muc_status_codes: [110] });
    await fanOut;
    expect(joinRoom.mock.calls.map((call) => (call as unknown[])[0])).toEqual([REMOTE_ROOM, "general@muc.example.com"]);
  });

  test("the room's status codes reach departure listeners", () => {
    const { client, deliver } = connectedClient();
    const departures: Array<[string, readonly number[]]> = [];
    client.onOwnRoomDeparture((roomJid, codes) => departures.push([roomJid, codes]));
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "unavailable", muc_status_codes: [110, 333] });
    expect(departures).toEqual([[REMOTE_ROOM, [110, 333]]]);
  });
});

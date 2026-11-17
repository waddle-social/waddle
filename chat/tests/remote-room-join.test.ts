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
import { HOME_FEDERATION_FEATURE } from "../src/lib/xmpp/discovery";
import { nullResumePersistence, type ResumePersistence } from "../src/lib/xmpp/resume-persistence";
import { discoInfoXml, discoItemsXml, pubsubItemsXml, withFakeDomParser } from "./helpers/disco-xml";

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
  muc_destroyed?: boolean;
  error_type?: string;
  error_condition?: string;
};

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  wireEvents: (xmpp: unknown) => void;
  retainedJoinedRoomJids: Set<string>;
  autoJoinRoomJids: ReadonlyArray<string>;
  fullJid: string;
  homeMucService: string | null;
  fanOutJoinableRooms: () => void;
  roomJoinRetry: { pending: (key: string) => Promise<void> | undefined };
};

/**
 * Record the delay of every timer of a second or more and never fire it,
 * so a join's self-presence budget can be read and its expiry run by hand.
 * Shorter timers (the tests' own ticks) run as usual.
 */
function captureLongTimers() {
  const original = globalThis.setTimeout;
  const long: Array<{ delayMs: number; callback: () => void }> = [];
  globalThis.setTimeout = ((callback: () => void, delayMs?: number, ...rest: unknown[]) => {
    if ((delayMs ?? 0) >= 1_000) {
      long.push({ delayMs: delayMs ?? 0, callback });
      return 0 as unknown as ReturnType<typeof setTimeout>;
    }
    return original(callback, delayMs, ...rest);
  }) as typeof setTimeout;
  return { long, restore: () => { globalThis.setTimeout = original; } };
}

function connectedClient(options: { homeMucService?: string | null } = {}) {
  const client = new BrowserXmppClient(session());
  const state = client as unknown as PrivateState;
  // As after a discovery that found the home MUC service.
  state.homeMucService = options.homeMucService === undefined ? "muc.example.com" : options.homeMucService;
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

  test("no room is remote before the home MUC service is discovered", () => {
    const { client } = connectedClient({ homeMucService: null });
    expect(client.isRemoteRoom(REMOTE_ROOM)).toBe(false);
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

  test("a remote join waits past the home server's worst case; a home join keeps its budget", async () => {
    const { client } = connectedClient();
    const timers = captureLongTimers();
    try {
      void client.ensureJoined(REMOTE_ROOM).catch(() => undefined);
      void client.ensureJoined("general@muc.example.com").catch(() => undefined);
      await Promise.resolve();
      await Promise.resolve();
      expect(timers.long.map((timer) => timer.delayMs)).toEqual([150_000, 15_000]);
    } finally {
      timers.restore();
    }
  });

  test("before the MUC service is discovered every join gets the home budget", async () => {
    const { client } = connectedClient({ homeMucService: null });
    const timers = captureLongTimers();
    try {
      void client.ensureJoined(REMOTE_ROOM).catch(() => undefined);
      await Promise.resolve();
      await Promise.resolve();
      expect(timers.long.map((timer) => timer.delayMs)).toEqual([15_000]);
    } finally {
      timers.restore();
    }
  });

  test("the room's answer to a join that timed out here replaces the retry", async () => {
    const { client, state, deliver } = connectedClient();
    const timers = captureLongTimers();
    try {
      state.autoJoinRoomJids = [REMOTE_ROOM];
      const failures: Array<string | undefined> = [];
      client.onRoomJoinFailed((_roomJid, error) => failures.push(error.timedOut ? "timeout" : error.condition));
      const join = client.ensureJoined(REMOTE_ROOM).catch((caught: unknown) => caught);
      await Promise.resolve();
      await Promise.resolve();
      timers.long.find((timer) => timer.delayMs === 150_000)!.callback();
      expect(await join).toMatchObject({ timedOut: true });
      expect(state.roomJoinRetry.pending(REMOTE_ROOM)).toBeDefined();

      deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "error", error_type: "auth", error_condition: "forbidden" });
      expect(failures).toEqual(["timeout", "forbidden"]);
      expect(state.roomJoinRetry.pending(REMOTE_ROOM)).toBeUndefined();
      expect(client.listRoomAccessRequirements()).toEqual([{ roomJid: REMOTE_ROOM, state: "required", condition: "forbidden" }]);
    } finally {
      timers.restore();
    }
  });

  test("a remote join that created the room is left at once and fails as a missing room", async () => {
    const { client, state, leaveRoom, deliver } = connectedClient();
    const join = client.ensureJoined(REMOTE_ROOM);
    await Promise.resolve();
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "available", muc_status_codes: [110, 201] });
    const error = await join.catch((caught: unknown) => caught);
    expect(error).toMatchObject({ roomCreated: true, condition: "item-not-found" });
    expect(classifyRemoteError(error).kind).toBe("deletion");
    expect(leaveRoom).toHaveBeenCalledWith(REMOTE_ROOM, "alice");
    expect(state.retainedJoinedRoomJids.has(REMOTE_ROOM)).toBe(false);
  });

  test("a home join that created the room completes as before", async () => {
    const { client, leaveRoom, deliver } = connectedClient();
    const join = client.ensureJoined("fresh@muc.example.com");
    await Promise.resolve();
    deliver({ from: "fresh@muc.example.com/alice", presence_type: "available", muc_status_codes: [110, 201] });
    await join;
    expect(leaveRoom).not.toHaveBeenCalled();
  });

  test("a destroyed room ends the occupancy and is not joined again", async () => {
    const { client, state, deliver } = connectedClient();
    const departures: Array<boolean> = [];
    client.onOwnRoomDeparture((_roomJid, _codes, departure) => departures.push(departure.destroyed));
    const join = client.ensureJoined(REMOTE_ROOM);
    await Promise.resolve();
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "available", muc_status_codes: [110] });
    await join;
    // XEP-0045 §10.9: no status 110 and no real JID on the removal.
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "unavailable", muc_destroyed: true });
    expect(departures).toEqual([true]);
    expect(state.retainedJoinedRoomJids.has(REMOTE_ROOM)).toBe(false);
  });

  test("background joins after a lost stream put this deployment's rooms first", async () => {
    const { state, joinRoom } = connectedClient();
    state.retainedJoinedRoomJids = new Set([REMOTE_ROOM, "general@muc.example.com"]);
    state.fanOutJoinableRooms();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(joinRoom.mock.calls.map((call) => (call as unknown[])[0])).toEqual(["general@muc.example.com", REMOTE_ROOM]);
  });

});

describe("sign-in joins of rooms on other servers", () => {
  /**
   * A home deployment `example.com` whose Space lists a home room and a
   * room on another server, and whose user bookmarked a third room there.
   */
  function signedInClient(options: { federation: boolean }) {
    const joinRoom = mock(async () => undefined);
    const xmpp = {
      join_room: joinRoom,
      set_on_presence() {},
      async send_raw_iq(xml: string): Promise<string> {
        if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
          if (xml.includes('to="example.com"')) {
            return discoItemsXml([{ jid: "muc.example.com" }, { jid: "spaces.example.com" }]);
          }
          if (xml.includes('to="spaces.example.com"')) return discoItemsXml([{ name: "Team", node: "team" }]);
          if (xml.includes('to="muc.example.com"')) return discoItemsXml([{ jid: "general@muc.example.com", name: "General" }]);
          return discoItemsXml([]);
        }
        if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
          if (xml.includes("remote.test")) throw new Error(`no request to another server was expected: ${xml}`);
          if (xml.includes('to="example.com"')) {
            return discoInfoXml({
              identities: [{ category: "server", type: "im" }],
              features: options.federation ? [HOME_FEDERATION_FEATURE] : [],
            });
          }
          if (xml.includes('to="spaces.example.com"')) {
            return discoInfoXml({
              identities: [{ category: "pubsub", type: xml.includes(" node=") ? "leaf" : "service" }],
              features: ["http://jabber.org/protocol/pubsub", "urn:xmpp:spaces:0"],
              fields: { FORM_TYPE: "http://jabber.org/protocol/pubsub#meta-data", "pubsub#type": "urn:xmpp:spaces:0" },
            });
          }
          return discoInfoXml({
            identities: [{ category: "conference", type: "text" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        if (xml.includes('to="spaces.example.com"')) {
          return pubsubItemsXml([
            { id: "general@muc.example.com", name: "General", autojoin: true },
            { id: "lobby@chat.remote.test", name: "Lobby", autojoin: true },
          ]);
        }
        return pubsubItemsXml([{ id: "dev@chat.remote.test", name: "Dev", autojoin: true }]);
      },
    };
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    state.xmpp = xmpp;
    state.connected = true;
    return { client, joinRoom };
  }

  async function joinedAfterSignIn(federation: boolean): Promise<string[]> {
    const { client, joinRoom } = signedInClient({ federation });
    await withFakeDomParser(async () => {
      await client.discoverTopology();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    return joinRoom.mock.calls.map((call) => (call as unknown[])[0] as string);
  }

  test("a Space never makes the client join a room on another server; the user's own bookmark does", async () => {
    const joined = await joinedAfterSignIn(true);
    expect(joined).not.toContain("lobby@chat.remote.test");
    // This deployment's rooms are joined first.
    expect(joined).toEqual(["general@muc.example.com", "dev@chat.remote.test"]);
  });

  test("while the home server does not federate, not even the user's bookmark joins a room elsewhere", async () => {
    const joined = await joinedAfterSignIn(false);
    expect(joined).toEqual(["general@muc.example.com"]);
  });
});

/**
 * Browser storage shared by successive clients of one account, as a page
 * reload or the next sign-in in the same browser sees it.
 */
function sharedStorage(initial: { joinedRooms?: string[]; missingRemoteRooms?: string[] } = {}) {
  const stored = {
    joinedRooms: [...(initial.joinedRooms ?? [])],
    missingRemoteRooms: [...(initial.missingRemoteRooms ?? [])],
  };
  const persistence: ResumePersistence = {
    ...nullResumePersistence,
    loadJoinedRooms: () => [...stored.joinedRooms],
    saveJoinedRooms: (roomJids) => { stored.joinedRooms = [...roomJids]; },
    loadMissingRemoteRooms: () => [...stored.missingRemoteRooms],
    saveMissingRemoteRooms: (roomJids) => { stored.missingRemoteRooms = [...roomJids]; },
  };
  return { stored, persistence };
}

/**
 * A home deployment `example.com` (MUC service `muc.example.com`, unless
 * its listing fails) whose user's bookmarks the test chooses, wired so the
 * test can deliver room presences.
 */
function homeDeployment(options: {
  persistence?: ResumePersistence;
  bookmarks?: Array<{ id: string; name?: string; autojoin?: boolean }>;
  catalog?: Array<{ jid: string; name?: string }>;
}) {
  const config = {
    federation: true,
    serverItemsFail: false,
    catalog: options.catalog ?? [{ jid: "general@muc.example.com", name: "General" }],
    bookmarks: options.bookmarks ?? [],
  };
  const sent: string[] = [];
  let onPresence: ((presence: Presence) => void) | null = null;
  const joinRoom = mock(async () => undefined);
  const leaveRoom = mock(async () => undefined);
  const xmpp = {
    join_room: joinRoom,
    leave_room: leaveRoom,
    set_on_presence(cb: (presence: Presence) => void) {
      onPresence = cb;
    },
    async send_raw_iq(xml: string): Promise<string> {
      sent.push(xml);
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.com"')) {
          if (config.serverItemsFail) throw new Error("server items unavailable");
          return discoItemsXml([{ jid: "muc.example.com" }]);
        }
        if (xml.includes('to="muc.example.com"')) return discoItemsXml(config.catalog);
        return discoItemsXml([]);
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (xml.includes("remote.test") || xml.includes("elsewhere.test")) {
          throw new Error(`no request to another server was expected: ${xml}`);
        }
        if (xml.includes('to="example.com"')) {
          return discoInfoXml({
            identities: [{ category: "server", type: "im" }],
            features: config.federation ? [HOME_FEDERATION_FEATURE] : [],
          });
        }
        return discoInfoXml({
          identities: [{ category: "conference", type: "text" }],
          features: ["http://jabber.org/protocol/muc"],
        });
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) return pubsubItemsXml(config.bookmarks);
      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
  const client = new BrowserXmppClient(session(), options.persistence);
  const state = client as unknown as PrivateState;
  state.xmpp = xmpp;
  state.connected = true;
  state.wireEvents(xmpp);
  const joined = () => joinRoom.mock.calls.map((call) => (call as unknown[])[0] as string);
  async function discover() {
    await withFakeDomParser(async () => {
      await client.discoverTopology();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
  return {
    client,
    state,
    config,
    sent,
    joinRoom,
    leaveRoom,
    joined,
    discover,
    deliver: (presence: Presence) => onPresence?.(presence),
  };
}

describe("a bookmarked room on another server that does not exist", () => {
  const bookmarks = [{ id: REMOTE_ROOM, name: "Dev", autojoin: true }];

  test("is joined once, left, and not joined on its own at the next sign-in", async () => {
    const storage = sharedStorage();
    const first = homeDeployment({ persistence: storage.persistence, bookmarks });
    await first.discover();
    expect(first.joined()).toContain(REMOTE_ROOM);
    // Its host creates the room for the join (XEP-0045 status 201).
    first.deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "available", muc_status_codes: [110, 201] });
    expect(first.leaveRoom).toHaveBeenCalledWith(REMOTE_ROOM, "alice");
    expect(storage.stored.missingRemoteRooms).toEqual([REMOTE_ROOM]);

    // The next sign-in: the bookmark still says to join (the write that
    // turns it off may not have reached the server), and nothing joins it.
    const second = homeDeployment({ persistence: storage.persistence, bookmarks });
    await second.discover();
    expect(second.joined()).toEqual(["general@muc.example.com"]);
    expect(second.leaveRoom).not.toHaveBeenCalled();
  });

  test("a join the user starts is still sent, and finding the room ends the record", async () => {
    const storage = sharedStorage({ missingRemoteRooms: [REMOTE_ROOM] });
    const { client, joined, deliver } = homeDeployment({ persistence: storage.persistence, bookmarks });
    const join = client.ensureJoined(REMOTE_ROOM);
    await Promise.resolve();
    expect(joined()).toEqual([REMOTE_ROOM]);
    deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "available", muc_status_codes: [110] });
    await join;
    expect(storage.stored.missingRemoteRooms).toEqual([]);
  });

  test("a bookmark whose autojoin was turned off is not joined at sign-in", async () => {
    const { discover, joined } = homeDeployment({ bookmarks: [{ id: REMOTE_ROOM, name: "Dev", autojoin: false }] });
    await discover();
    expect(joined()).not.toContain(REMOTE_ROOM);
  });
});

describe("rooms whose host is not known yet", () => {
  test("a bookmark of a room elsewhere is neither asked nor joined while the home MUC service is unknown", async () => {
    const deployment = homeDeployment({ bookmarks: [{ id: REMOTE_ROOM, name: "Dev", autojoin: true }] });
    deployment.config.serverItemsFail = true;
    await deployment.discover();
    expect(deployment.joined()).not.toContain(REMOTE_ROOM);
    expect(deployment.sent.some((xml) => xml.includes("remote.test"))).toBe(false);
    // Nothing placed the room at home either.
    expect(deployment.state.homeMucService).toBeNull();

    // A later discovery that finds the service places it, and the user's
    // bookmark joins it while the home server federates.
    deployment.config.serverItemsFail = false;
    await deployment.discover();
    expect(deployment.joined()).toContain(REMOTE_ROOM);
  });

  test("a retained room is rejoined at session-ready only on the account domain; discovery places the others", async () => {
    const storage = sharedStorage({
      joinedRooms: ["lounge@example.com", "quiet@muc.example.com", "old@chat.elsewhere.test"],
    });
    const deployment = homeDeployment({ persistence: storage.persistence });
    deployment.state.fanOutJoinableRooms();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(deployment.joined()).toEqual(["lounge@example.com"]);
    // A page hidden before discovery keeps the parked rooms for the next page.
    (deployment.client as unknown as { persistRetainedJoinedRooms(): void }).persistRetainedJoinedRooms();
    expect(storage.stored.joinedRooms).toContain("quiet@muc.example.com");
    expect(storage.stored.joinedRooms).toContain("old@chat.elsewhere.test");

    await deployment.discover();
    expect(deployment.joined()).toContain("quiet@muc.example.com");
    expect(deployment.joined()).not.toContain("old@chat.elsewhere.test");
    expect(storage.stored.joinedRooms).not.toContain("old@chat.elsewhere.test");
    expect(storage.stored.joinedRooms).toContain("quiet@muc.example.com");
  });

  test("a room is placed at home by the latest catalog read, not by every catalog ever read", async () => {
    const odd = "odd@rooms.example.org";
    const deployment = homeDeployment({ catalog: [{ jid: odd, name: "Odd" }] });
    await deployment.discover();
    expect(deployment.client.isRemoteRoom(odd)).toBe(false);

    deployment.config.catalog = [];
    await deployment.discover();
    expect(deployment.client.isRemoteRoom(odd)).toBe(true);
  });
});

describe("the home server stops federating", () => {
  test("rooms on other servers the session is in are left, and home rooms are kept", async () => {
    const deployment = homeDeployment({ bookmarks: [{ id: REMOTE_ROOM, name: "Dev", autojoin: true }] });
    await deployment.discover();
    deployment.deliver({ from: `${REMOTE_ROOM}/alice`, presence_type: "available", muc_status_codes: [110] });
    deployment.deliver({ from: "general@muc.example.com/alice", presence_type: "available", muc_status_codes: [110] });

    deployment.config.federation = false;
    await deployment.discover();
    const left = deployment.leaveRoom.mock.calls.map((call) => (call as unknown[])[0]);
    expect(left).toEqual([REMOTE_ROOM]);
    expect(deployment.state.retainedJoinedRoomJids.has(REMOTE_ROOM)).toBe(false);
  });
});

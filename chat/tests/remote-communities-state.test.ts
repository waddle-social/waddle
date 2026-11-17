import { describe, expect, test } from "bun:test";
import { computed, ref, shallowRef } from "vue";
import { useRemoteCommunities } from "../src/waddles/remote-communities";
import { RoomJoinError } from "../src/lib/xmpp/room-join-error";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { JoinedSpacePreference, JoinedSpaceStore } from "../src/lib/xmpp/joined-spaces";
import { discoInfoXml, withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

const ROOM = "dev@chat.remote.test";
const HOME = { accountDomain: "home.test", mucService: "muc.home.test", spacesService: "spaces.home.test" };

function withXml(run: () => Promise<void>): Promise<void> {
  return withFakeDomParser(() => withFakeXmlDocument(run));
}

function stanzaError(condition: string, errorType = "cancel"): Error {
  return Object.assign(new Error(condition), { condition, errorType });
}

function manualTimer() {
  const pending: Array<{ callback: () => void; delayMs: number }> = [];
  return {
    pending,
    timer: {
      setTimeout(callback: () => void, delayMs: number) {
        pending.push({ callback, delayMs });
        return pending.length as unknown as ReturnType<typeof setTimeout>;
      },
      clearTimeout() {},
    },
    runNext() {
      pending.shift()?.callback();
    },
  };
}

function memoryStore(initial: JoinedSpacePreference[] = []) {
  const state = { preferences: [...initial], saves: 0 };
  const store: JoinedSpaceStore = {
    async load() { return [...state.preferences]; },
    async save(preferences) { state.saves += 1; state.preferences = [...preferences]; },
  };
  return { state, store };
}

function fakeClient(options: { joinErrors?: Error[]; bookmarks?: Array<{ jid: string; name: string | null; autojoin: boolean }> } = {}) {
  const sent: string[] = [];
  const joined: string[] = [];
  const left: string[] = [];
  const departures: Array<(roomJid: string, codes: readonly number[]) => void> = [];
  const joinFailures: Array<(roomJid: string, error: RoomJoinError) => void> = [];
  const nicks: Array<(roomJid: string, nick: string | null) => void> = [];
  const joinErrors = [...(options.joinErrors ?? [])];
  const agent = {
    async send_raw_iq(xml: string): Promise<string> {
      sent.push(xml);
      if (xml.includes('node="urn:xmpp:bookmarks:1"') && xml.includes('type="get"')) throw stanzaError("item-not-found");
      if (xml.includes("disco#info")) {
        return discoInfoXml({
          identities: [{ category: "pubsub", type: "leaf" }],
          fields: { FORM_TYPE: "http://jabber.org/protocol/pubsub#meta-data", "pubsub#type": "urn:xmpp:spaces:0", "pubsub#title": "Engineering" },
        });
      }
      if (xml.includes("<items ")) {
        return '<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="eng"></items></pubsub></iq>';
      }
      return '<iq type="result"/>';
    },
  };
  const client = {
    agent,
    async ensureJoined(roomJid: string) {
      joined.push(roomJid);
      const error = joinErrors.shift();
      if (error) throw error;
      for (const handler of nicks) handler(roomJid, "alice");
    },
    async leaveRoom(roomJid: string) {
      left.push(roomJid);
      for (const handler of departures) handler(roomJid, [110]);
    },
    async fetchUserBookmarks() {
      return (options.bookmarks ?? []).map((bookmark) => ({ ...bookmark, notifyMode: null, richPayloadOptIn: false }));
    },
    onOwnRoomDeparture(handler: (roomJid: string, codes: readonly number[]) => void) {
      departures.push(handler);
      return () => undefined;
    },
    onRoomJoinFailed(handler: (roomJid: string, error: RoomJoinError) => void) {
      joinFailures.push(handler);
      return () => undefined;
    },
    addOwnOccupantNickHandler(handler: (roomJid: string, nick: string | null) => void) {
      nicks.push(handler);
      return () => undefined;
    },
  };
  return {
    client: client as unknown as BrowserXmppClient,
    sent,
    joined,
    left,
    depart: (roomJid: string, codes: number[]) => { for (const handler of departures) handler(roomJid, codes); },
    failJoin: (roomJid: string, error: RoomJoinError) => { for (const handler of joinFailures) handler(roomJid, error); },
  };
}

function setup(
  options: Parameters<typeof fakeClient>[0] & { roomListed?: boolean } = {},
  preferences: JoinedSpacePreference[] = [],
) {
  const fake = fakeClient(options);
  const timer = manualTimer();
  const memory = memoryStore(preferences);
  const opened: string[] = [];
  const closed: string[] = [];
  let reloads = 0;
  const remote = useRemoteCommunities({
    xmppClient: shallowRef<BrowserXmppClient | null>(fake.client),
    session: ref({ jid: "alice@home.test" } as WaddleSession),
    home: computed(() => HOME),
    reloadRooms: async () => { reloads += 1; },
    openRoom: async (roomJid) => {
      if (options.roomListed === false) return false;
      opened.push(roomJid);
      return true;
    },
    closeRoom: (roomJid) => { closed.push(roomJid); },
    createStore: () => memory.store,
    timer: timer.timer,
    now: () => 0,
  });
  return { remote, fake, timer, memory, opened, closed, reloads: () => reloads };
}

function roomEntry(remote: ReturnType<typeof setup>["remote"], roomJid = ROOM) {
  return remote.rooms.value.find((room) => room.roomJid === roomJid);
}

describe("joining a room on another server", () => {
  test("a seated join is bookmarked to join at sign-in, and the room opens", async () => {
    await withXml(async () => {
      const { remote, fake, opened, reloads } = setup();
      await remote.joinAndOpenRoom(ROOM, "Developers");
      expect(fake.joined).toEqual([ROOM]);
      const publish = fake.sent.find((xml) => xml.includes("<publish "));
      expect(publish).toContain(`<item id="${ROOM}">`);
      expect(publish).toContain('autojoin="true"');
      expect(roomEntry(remote)).toMatchObject({ bookmarked: true, autojoin: true, joined: true, status: { state: "ready" } });
      expect(reloads()).toBe(1);
      expect(opened).toEqual([ROOM]);
    });
  });

  test("a denial is shown and never retried on its own, and nothing is bookmarked", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup({
        joinErrors: [new RoomJoinError("no", { roomJid: ROOM, condition: "registration-required", errorType: "auth" })],
      });
      await remote.joinRoom(ROOM);
      expect(roomEntry(remote)?.status).toMatchObject({ state: "failed", failure: { kind: "denial" }, retryAt: null });
      expect(timer.pending).toHaveLength(0);
      expect(fake.sent.some((xml) => xml.includes("<publish "))).toBe(false);
    });
  });

  test("a room its host does not know is shown as unavailable, without a retry", async () => {
    await withXml(async () => {
      const { remote, timer } = setup({
        joinErrors: [new RoomJoinError("no", { roomJid: ROOM, condition: "item-not-found", errorType: "cancel" })],
      });
      await remote.joinRoom(ROOM);
      expect(roomEntry(remote)?.status).toMatchObject({ state: "failed", failure: { kind: "deletion", retry: "none" } });
      expect(timer.pending).toHaveLength(0);
    });
  });

  test("an outage is retried with backoff until the host answers", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup({
        joinErrors: [new RoomJoinError("slow", { roomJid: ROOM, condition: "remote-server-timeout", errorType: "wait" })],
      });
      await remote.joinRoom(ROOM);
      expect(roomEntry(remote)?.status).toMatchObject({ state: "failed", failure: { kind: "outage", retry: "automatic" } });
      expect(timer.pending).toHaveLength(1);
      expect(timer.pending[0]!.delayMs).toBeGreaterThanOrEqual(2_000);
      timer.runNext();
      await Promise.resolve();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(fake.joined).toEqual([ROOM, ROOM]);
    });
  });
});

describe("leave, forget and revoke for rooms", () => {
  test("leaving ends the occupancy and turns autojoin off, keeping the bookmark", async () => {
    await withXml(async () => {
      const { remote, fake, closed } = setup();
      await remote.joinRoom(ROOM, "Developers");
      fake.sent.length = 0;
      await remote.leaveRoom(ROOM);
      expect(fake.left).toEqual([ROOM]);
      const republish = fake.sent.find((xml) => xml.includes("<publish "));
      expect(republish).toBeDefined();
      expect(republish).not.toContain("autojoin=");
      expect(fake.sent.some((xml) => xml.includes("<retract "))).toBe(false);
      expect(roomEntry(remote)).toMatchObject({ bookmarked: true, autojoin: false, joined: false });
      // A left room stops being shown, since showing a room joins it.
      expect(closed).toEqual([ROOM]);
    });
  });

  test("a room that cannot be opened yet says why instead of doing nothing", async () => {
    await withXml(async () => {
      const { remote, opened } = setup({ roomListed: false });
      await remote.joinAndOpenRoom(ROOM, "Developers");
      expect(opened).toEqual([]);
      expect(roomEntry(remote)?.note).toContain("can't be opened");
    });
  });

  test("a joined room is not forgotten; once left, forgetting removes only the bookmark", async () => {
    await withXml(async () => {
      const { remote, fake } = setup();
      await remote.joinRoom(ROOM);
      fake.sent.length = 0;
      await remote.forgetRoom(ROOM);
      expect(fake.sent).toHaveLength(0);
      await remote.leaveRoom(ROOM);
      fake.sent.length = 0;
      await remote.forgetRoom(ROOM);
      expect(fake.sent.some((xml) => xml.includes('<retract node="urn:xmpp:bookmarks:1" notify="true">'))).toBe(true);
      expect(fake.left).toEqual([ROOM]);
      expect(roomEntry(remote)).toBeUndefined();
    });
  });

  test("a revocation by the host keeps the bookmark and waits for the user", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup();
      await remote.joinRoom(ROOM);
      fake.sent.length = 0;
      fake.depart(ROOM, [110, 301]);
      expect(roomEntry(remote)).toMatchObject({
        departure: "revoked",
        bookmarked: true,
        joined: false,
        status: { state: "failed", failure: { kind: "denial", retry: "manual" } },
      });
      expect(fake.sent).toHaveLength(0);
      expect(timer.pending).toHaveLength(0);
    });
  });

  test("a lost federation stream rejoins with backoff and leaves the bookmark as it was", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup();
      await remote.joinRoom(ROOM);
      fake.depart(ROOM, [110, 333]);
      expect(roomEntry(remote)).toMatchObject({ departure: "outage", status: { state: "failed", failure: { kind: "outage" } } });
      expect(timer.pending).toHaveLength(1);
      fake.sent.length = 0;
      timer.runNext();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(fake.joined).toEqual([ROOM, ROOM]);
      expect(fake.sent.some((xml) => xml.includes("<publish "))).toBe(false);
      expect(roomEntry(remote)?.status).toEqual({ state: "ready" });
    });
  });

  test("a join the client started elsewhere shows its failure without a second retry loop", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup({ bookmarks: [{ jid: ROOM, name: "Developers", autojoin: true }] });
      await remote.load();
      fake.failJoin(ROOM, new RoomJoinError("slow", { roomJid: ROOM, condition: "remote-server-timeout", errorType: "wait" }));
      expect(roomEntry(remote)?.status).toMatchObject({ state: "failed", failure: { kind: "outage" }, retryAt: null });
      expect(timer.pending).toHaveLength(0);
    });
  });

  test("a room on this deployment is not handled as a remote departure", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup();
      fake.depart("general@muc.home.test", [110, 333]);
      expect(remote.rooms.value).toHaveLength(0);
      expect(timer.pending).toHaveLength(0);
    });
  });
});

describe("following, unfollowing and forgetting Spaces", () => {
  test("following subscribes on the host and then records the Space in the joined-space list", async () => {
    await withXml(async () => {
      const { remote, fake, memory } = setup();
      await remote.followSpace("spaces.remote.test", "eng", "Engineering");
      expect(fake.sent.some((xml) => xml.includes('<subscribe node="eng" jid="alice@home.test">'))).toBe(true);
      expect(memory.state.preferences).toEqual([{ serviceJid: "spaces.remote.test", node: "eng", name: "Engineering", follow: true }]);
      expect(remote.spaces.value[0]).toMatchObject({ listed: true, following: true });
    });
  });

  test("a refused subscription writes no list entry", async () => {
    await withXml(async () => {
      const { remote, fake, memory } = setup();
      const send = fake.client.agent as unknown as { send_raw_iq: (xml: string) => Promise<string> };
      send.send_raw_iq = async () => { throw stanzaError("forbidden", "auth"); };
      await remote.followSpace("spaces.remote.test", "eng");
      expect(memory.state.saves).toBe(0);
      expect(remote.spaces.value[0]?.status).toMatchObject({ state: "failed", failure: { kind: "denial" } });
    });
  });

  test("unfollowing ends the subscription and keeps the Space listed; forgetting waits for it", async () => {
    await withXml(async () => {
      const { remote, fake, memory } = setup({}, [{ serviceJid: "spaces.remote.test", node: "eng", follow: true }]);
      await remote.load();
      // Let the renewal of the followed Space's subscription settle.
      await new Promise((resolve) => setTimeout(resolve, 0));
      const key = remote.spaces.value[0]!.key;

      await remote.forgetSpace(key);
      expect(memory.state.preferences).toHaveLength(1);

      await remote.unsubscribeSpace(key);
      expect(fake.sent.some((xml) => xml.includes('<unsubscribe node="eng" jid="alice@home.test">'))).toBe(true);
      expect(memory.state.preferences).toEqual([{ serviceJid: "spaces.remote.test", node: "eng", follow: false }]);
      expect(remote.spaces.value[0]).toMatchObject({ listed: true, following: false });

      await remote.forgetSpace(key);
      expect(memory.state.preferences).toEqual([]);
      expect(remote.spaces.value).toHaveLength(0);
    });
  });

  test("a listed Space carries no role: whatever the list says, the host is asked", async () => {
    await withXml(async () => {
      const { remote, fake } = setup({}, [{ serviceJid: "spaces.remote.test", node: "eng", follow: false }]);
      await remote.load();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(Object.keys(remote.spaces.value[0]!)).not.toContain("role");
      expect(fake.sent.some((xml) => xml.includes('to="spaces.remote.test"'))).toBe(true);
    });
  });
});

describe("looking up an address", () => {
  test("an address of this deployment is not looked up as a remote community", async () => {
    await withXml(async () => {
      const { remote, fake } = setup();
      await remote.lookUp("general@muc.home.test");
      expect(remote.lookup.value.state).toBe("home");
      expect(fake.sent).toHaveLength(0);
    });
  });

  test("an unusable address is explained without a request", async () => {
    await withXml(async () => {
      const { remote, fake } = setup();
      await remote.lookUp("room@127.0.0.1");
      expect(remote.lookup.value).toEqual({ state: "invalid", error: "refused-domain" });
      expect(fake.sent).toHaveLength(0);
    });
  });
});

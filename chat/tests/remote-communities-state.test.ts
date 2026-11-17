import { describe, expect, test } from "bun:test";
import { computed, ref, shallowRef } from "vue";
import { useRemoteCommunities } from "../src/waddles/remote-communities";
import { RoomJoinError } from "../src/lib/xmpp/room-join-error";
import { describeRemoteFailure } from "../src/lib/xmpp/remote-outcome";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import {
  joinedSpaceKey,
  type JoinedSpacePreference,
  type JoinedSpaceStore,
  type SpaceUnfollowIntent,
  type SpaceUnfollowIntentStore,
} from "../src/lib/xmpp/joined-spaces";
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
  const state = { preferences: [...initial], saves: 0, failWrites: false };
  const store: JoinedSpaceStore = {
    async load() { return [...state.preferences]; },
    async put(preference) {
      if (state.failWrites) throw stanzaError("internal-server-error", "wait");
      state.saves += 1;
      const key = joinedSpaceKey(preference);
      const index = state.preferences.findIndex((candidate) => joinedSpaceKey(candidate) === key);
      state.preferences = index < 0
        ? [...state.preferences, preference]
        : state.preferences.map((candidate, position) => (position === index ? preference : candidate));
    },
    async remove(preference) {
      if (state.failWrites) throw stanzaError("internal-server-error", "wait");
      state.saves += 1;
      const key = joinedSpaceKey(preference);
      state.preferences = state.preferences.filter((candidate) => joinedSpaceKey(candidate) !== key);
    },
  };
  return { state, store };
}

function memoryIntents(initial: SpaceUnfollowIntent[] = []) {
  const state = { intents: [...initial] };
  const store: SpaceUnfollowIntentStore = {
    load: () => [...state.intents],
    save: (intents) => { state.intents = [...intents]; },
  };
  return { state, store };
}

type Departure = (roomJid: string, codes: readonly number[], details?: { destroyed: boolean }) => void;

function fakeClient(options: {
  joinErrors?: Error[];
  bookmarks?: Array<{ jid: string; name: string | null; autojoin: boolean }>;
  /** Fail every request whose XML contains this text with this error. */
  failRequests?: { match: string; error: Error };
  /** Joins wait until released. */
  holdJoins?: boolean;
  /** XEP-0402 items already stored, by item id: the `<conference/>` each holds. */
  storedBookmarks?: Record<string, string>;
} = {}) {
  const sent: string[] = [];
  // The user's bookmark node: publishes store their item, retracts remove it.
  const storedBookmarks = new Map(Object.entries(options.storedBookmarks ?? {}));
  const joined: string[] = [];
  const left: string[] = [];
  const departures: Departure[] = [];
  const joinFailures: Array<(roomJid: string, error: RoomJoinError) => void> = [];
  const joinStarts: Array<(roomJid: string) => void> = [];
  const nicks: Array<(roomJid: string, nick: string | null) => void> = [];
  const joinErrors = [...(options.joinErrors ?? [])];
  const heldJoins: Array<() => void> = [];
  const agent = {
    async send_raw_iq(xml: string): Promise<string> {
      sent.push(xml);
      if (options.failRequests && xml.includes(options.failRequests.match)) throw options.failRequests.error;
      if (xml.includes('node="urn:xmpp:bookmarks:1"') && xml.includes('type="get"')) {
        if (storedBookmarks.size === 0) throw stanzaError("item-not-found");
        const items = [...storedBookmarks].map(([id, conference]) => `<item id="${id}">${conference}</item>`).join("");
        return `<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="urn:xmpp:bookmarks:1">${items}</items></pubsub></iq>`;
      }
      if (xml.includes('<publish node="urn:xmpp:bookmarks:1">')) {
        const item = /<item id="([^"]+)">([\s\S]*?)<\/item>/.exec(xml);
        if (item) storedBookmarks.set(item[1]!, item[2]!);
      }
      if (xml.includes('<retract node="urn:xmpp:bookmarks:1"')) {
        const item = /<item id="([^"]+)"/.exec(xml.slice(xml.indexOf("<retract ")));
        if (item) storedBookmarks.delete(item[1]!);
      }
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
      for (const handler of joinStarts) handler(roomJid);
      if (options.holdJoins) await new Promise<void>((resolve) => { heldJoins.push(resolve); });
      const error = joinErrors.shift();
      if (error) throw error;
      for (const handler of nicks) handler(roomJid, "alice");
    },
    async leaveRoom(roomJid: string) {
      left.push(roomJid);
      for (const handler of departures) handler(roomJid, [110], { destroyed: false });
    },
    async fetchUserBookmarks() {
      return (options.bookmarks ?? []).map((bookmark) => ({ ...bookmark, notifyMode: null, richPayloadOptIn: false }));
    },
    onOwnRoomDeparture(handler: Departure) {
      departures.push(handler);
      return () => undefined;
    },
    onRoomJoinStarted(handler: (roomJid: string) => void) {
      joinStarts.push(handler);
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
    storedBookmarks,
    joined,
    left,
    depart: (roomJid: string, codes: number[], destroyed = false) => {
      for (const handler of departures) handler(roomJid, codes, { destroyed });
    },
    failJoin: (roomJid: string, error: RoomJoinError) => { for (const handler of joinFailures) handler(roomJid, error); },
    releaseJoins: () => { for (const release of heldJoins.splice(0)) release(); },
    startJoin: (roomJid: string) => { for (const handler of joinStarts) handler(roomJid); },
  };
}

async function settle(): Promise<void> {
  for (let index = 0; index < 5; index += 1) await new Promise((resolve) => setTimeout(resolve, 0));
}

function setup(
  options: Parameters<typeof fakeClient>[0] & {
    roomListed?: boolean;
    federation?: boolean;
    intents?: SpaceUnfollowIntent[];
    home?: typeof HOME;
  } = {},
  preferences: JoinedSpacePreference[] = [],
) {
  const fake = fakeClient(options);
  const timer = manualTimer();
  const memory = memoryStore(preferences);
  const intents = memoryIntents(options.intents);
  const federation = ref(options.federation ?? true);
  const opened: string[] = [];
  const closed: string[] = [];
  let reloads = 0;
  const remote = useRemoteCommunities({
    xmppClient: shallowRef<BrowserXmppClient | null>(fake.client),
    session: ref({ jid: "alice@home.test" } as WaddleSession),
    home: computed(() => options.home ?? HOME),
    federation,
    reloadRooms: async () => { reloads += 1; },
    openRoom: async (roomJid) => {
      if (options.roomListed === false) return false;
      opened.push(roomJid);
      return true;
    },
    closeRoom: (roomJid) => { closed.push(roomJid); },
    createStore: () => memory.store,
    createIntentStore: () => intents.store,
    timer: timer.timer,
    now: () => 0,
    random: () => 0.5,
  });
  return { remote, fake, timer, memory, intents, federation, opened, closed, reloads: () => reloads };
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
      expect(memory.state.preferences).toMatchObject([{ serviceJid: "spaces.remote.test", node: "eng", follow: false }]);
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

describe("rejoining after an outage", () => {
  test("a lost stream after the user left is no reason to join again", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup();
      await remote.joinRoom(ROOM);
      await remote.leaveRoom(ROOM);
      timer.pending.length = 0;
      fake.depart(ROOM, [110, 333]);
      expect(timer.pending).toHaveLength(0);
      expect(fake.joined).toEqual([ROOM]);
      expect(roomEntry(remote)?.departure).toBe("outage");
    });
  });

  test("a room that was not joined is not joined because of an outage", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup({ bookmarks: [{ jid: ROOM, name: "Developers", autojoin: false }] });
      await remote.load();
      fake.depart(ROOM, [110, 332]);
      expect(timer.pending).toHaveLength(0);
      expect(fake.joined).toEqual([]);
    });
  });

  test("a destroyed room is shown as gone and never joined again", async () => {
    await withXml(async () => {
      const { remote, fake, timer } = setup();
      await remote.joinRoom(ROOM);
      fake.depart(ROOM, [], true);
      expect(roomEntry(remote)).toMatchObject({
        departure: "destroyed",
        joined: false,
        status: { state: "failed", failure: { kind: "deletion", retry: "none" } },
      });
      expect(timer.pending).toHaveLength(0);
    });
  });
});

describe("one room, several user actions", () => {
  test("Forget is refused while a join of the room is under way, whoever started it", async () => {
    await withXml(async () => {
      const { remote, fake } = setup({ bookmarks: [{ jid: ROOM, name: "Developers", autojoin: true }] });
      await remote.load();
      // A sign-in autojoin of the room, started by the client.
      fake.startJoin(ROOM);
      expect(roomEntry(remote)?.joining).toBe(true);
      fake.sent.length = 0;
      await remote.forgetRoom(ROOM);
      expect(fake.sent.some((xml) => xml.includes("<retract "))).toBe(false);

      fake.failJoin(ROOM, new RoomJoinError("no", { roomJid: ROOM, condition: "forbidden", errorType: "auth" }));
      expect(roomEntry(remote)?.joining).toBe(false);
      await remote.forgetRoom(ROOM);
      expect(fake.sent.some((xml) => xml.includes("<retract "))).toBe(true);
    });
  });

  test("a Leave chosen while the join was under way wins: the join does not turn autojoin on", async () => {
    await withXml(async () => {
      const { remote, fake } = setup({ holdJoins: true });
      const join = remote.joinRoom(ROOM, "Developers");
      await settle();
      const leave = remote.leaveRoom(ROOM);
      fake.releaseJoins();
      await Promise.all([join, leave]);
      const publishes = fake.sent.filter((xml) => xml.includes("<publish "));
      expect(publishes.some((xml) => xml.includes('autojoin="true"'))).toBe(false);
    });
  });
});

describe("unfollowing when the host cannot confirm it", () => {
  test("an outage unfollows here, keeps the unsubscribe on this device, and lets the Space be forgotten", async () => {
    await withXml(async () => {
      const { remote, memory, intents } = setup(
        { failRequests: { match: "<unsubscribe ", error: stanzaError("remote-server-timeout", "wait") } },
        [{ serviceJid: "spaces.remote.test", node: "eng", follow: true }],
      );
      await remote.load();
      await settle();
      const key = remote.spaces.value[0]!.key;
      await remote.unsubscribeSpace(key);
      expect(remote.spaces.value[0]).toMatchObject({ following: false });
      expect(remote.spaces.value[0]!.note).toContain("next sign-in");
      expect(intents.state.intents).toEqual([
        { serviceJid: "spaces.remote.test", node: "eng", remoteDone: false, preferenceDone: true },
      ]);
      expect(memory.state.preferences).toMatchObject([{ follow: false }]);

      await remote.forgetSpace(key);
      expect(memory.state.preferences).toEqual([]);
      // The unsubscribe still waits to be told to the host.
      expect(intents.state.intents).toMatchObject([{ remoteDone: false }]);
    });
  });

  test("an unfollow whose list entry could not be written is not undone by the next sign-in", async () => {
    await withXml(async () => {
      const first = setup({}, [{ serviceJid: "spaces.remote.test", node: "eng", follow: true }]);
      await first.remote.load();
      await settle();
      first.memory.state.failWrites = true;
      await first.remote.unsubscribeSpace(first.remote.spaces.value[0]!.key);
      expect(first.intents.state.intents).toMatchObject([{ remoteDone: true, preferenceDone: false }]);
      // The list still says to follow.
      expect(first.memory.state.preferences).toMatchObject([{ follow: true }]);

      const next = setup({ intents: first.intents.state.intents }, first.memory.state.preferences);
      await settle();
      for (const entry of [...next.timer.pending]) entry.callback();
      next.timer.pending.length = 0;
      await settle();
      expect(next.fake.sent.some((xml) => xml.includes("<subscribe "))).toBe(false);
      expect(next.remote.spaces.value[0]).toMatchObject({ following: false });
      // The list entry is written at that sign-in, which settles the record.
      expect(next.memory.state.preferences).toMatchObject([{ follow: false }]);
      expect(next.intents.state.intents).toEqual([]);
    });
  });
});

describe("renewing followed Spaces at sign-in", () => {
  test("each followed Space is renewed once, spread out, and only while the home server federates", async () => {
    await withXml(async () => {
      const preferences = [
        { serviceJid: "spaces.remote.test", node: "eng", follow: true },
        { serviceJid: "spaces.remote.test", node: "ops", follow: true },
        { serviceJid: "spaces.remote.test", node: "old", follow: false },
      ];
      const { fake, timer, federation, remote } = setup({ federation: false }, preferences);
      await settle();
      expect(timer.pending).toHaveLength(0);
      expect(fake.sent).toHaveLength(0);

      federation.value = true;
      await settle();
      expect(timer.pending.map((entry) => entry.delayMs)).toEqual([1_500, 4_500]);
      for (const entry of timer.pending.splice(0)) entry.callback();
      await settle();
      const subscribes = fake.sent.filter((xml) => xml.includes("<subscribe "));
      expect(subscribes).toHaveLength(2);
      expect(subscribes.some((xml) => xml.includes('node="old"'))).toBe(false);

      // Opening the dialog reads content; it does not subscribe again.
      fake.sent.length = 0;
      await remote.load();
      await settle();
      expect(fake.sent.some((xml) => xml.includes("<subscribe "))).toBe(false);
    });
  });

  test("a denial is shown and not retried; an outage is retried with backoff", async () => {
    await withXml(async () => {
      const denied = setup(
        { failRequests: { match: "<subscribe ", error: stanzaError("forbidden", "auth") } },
        [{ serviceJid: "spaces.remote.test", node: "eng", follow: true }],
      );
      await settle();
      for (const entry of denied.timer.pending.splice(0)) entry.callback();
      await settle();
      expect(denied.remote.spaces.value[0]?.status).toMatchObject({ state: "failed", failure: { kind: "denial" }, retryAt: null });
      expect(denied.timer.pending).toHaveLength(0);

      const down = setup(
        { failRequests: { match: "<subscribe ", error: stanzaError("remote-server-timeout", "wait") } },
        [{ serviceJid: "spaces.remote.test", node: "eng", follow: true }],
      );
      await settle();
      for (const entry of down.timer.pending.splice(0)) entry.callback();
      await settle();
      expect(down.remote.spaces.value[0]?.status).toMatchObject({ state: "failed", failure: { kind: "outage" } });
      expect(down.timer.pending).toHaveLength(1);
    });
  });
});

describe("while the home MUC service is not discovered", () => {
  test("no room is taken for a remote one, so no entry is made", async () => {
    await withXml(async () => {
      const { remote, fake } = setup({
        home: { ...HOME, mucService: null as unknown as string },
        bookmarks: [{ jid: ROOM, name: "Developers", autojoin: true }],
      });
      await remote.load();
      fake.failJoin(ROOM, new RoomJoinError("slow", { roomJid: ROOM, timedOut: true }));
      fake.depart(ROOM, [110, 333]);
      expect(remote.rooms.value).toHaveLength(0);
    });
  });
});

const STORED_AUTOJOIN = '<conference xmlns="urn:xmpp:bookmarks:1" name="Developers" autojoin="true"/>';

function roomCreatedError(): RoomJoinError {
  return new RoomJoinError("This room doesn't exist on its server.", {
    roomJid: ROOM,
    condition: "item-not-found",
    errorType: "cancel",
    roomCreated: true,
  });
}

describe("a room that turned out not to exist", () => {
  test("its bookmark stops joining at sign-in, and the dialog says the room doesn't exist", async () => {
    await withXml(async () => {
      const { remote, fake, timer, reloads } = setup({ storedBookmarks: { [ROOM]: STORED_AUTOJOIN } });
      // A sign-in autojoin, started by the client, found no room.
      fake.failJoin(ROOM, roomCreatedError());
      await settle();
      const publish = fake.sent.find((xml) => xml.includes('<publish node="urn:xmpp:bookmarks:1">'));
      expect(publish).toBeDefined();
      expect(publish).not.toContain("autojoin=");
      // The bookmark stays, with its name.
      expect(fake.storedBookmarks.get(ROOM)).toContain('name="Developers"');
      expect(fake.storedBookmarks.get(ROOM)).not.toContain("autojoin=");
      const status = roomEntry(remote)?.status;
      expect(status).toMatchObject({ state: "failed", failure: { kind: "deletion", retry: "none", roomMissing: true } });
      if (status?.state !== "failed") throw new Error("expected a failed status");
      expect(describeRemoteFailure(status.failure, "room").title).toBe("This room doesn't exist");
      expect(timer.pending).toHaveLength(0);
      expect(reloads()).toBe(1);

      // The room's answer to the client leaving the room it created is
      // no outage: nothing joins it again.
      fake.depart(ROOM, [110]);
      await settle();
      expect(timer.pending).toHaveLength(0);
      expect(fake.joined).toEqual([]);
    });
  });

  test("a missing room the user never bookmarked is not bookmarked now", async () => {
    await withXml(async () => {
      const { fake } = setup();
      fake.failJoin(ROOM, roomCreatedError());
      await settle();
      expect(fake.sent.some((xml) => xml.includes("<publish "))).toBe(false);
      expect(fake.storedBookmarks.size).toBe(0);
    });
  });

  test("a join the user asked for that finds no room is shown as such, without a retry", async () => {
    await withXml(async () => {
      const { remote, timer } = setup({ joinErrors: [roomCreatedError()] });
      await remote.joinRoom(ROOM);
      expect(roomEntry(remote)?.status).toMatchObject({ state: "failed", failure: { kind: "deletion", roomMissing: true } });
      expect(timer.pending).toHaveLength(0);
    });
  });
});

describe("leaving reads the stored bookmark", () => {
  test("a room this device has not listed yet still stops joining at sign-in", async () => {
    await withXml(async () => {
      const { remote, fake } = setup({ storedBookmarks: { [ROOM]: STORED_AUTOJOIN } });
      expect(roomEntry(remote)).toBeUndefined();
      await remote.leaveRoom(ROOM);
      expect(fake.left).toEqual([ROOM]);
      expect(fake.storedBookmarks.get(ROOM)).not.toContain("autojoin=");
    });
  });

  test("leaving a room without a bookmark adds none", async () => {
    await withXml(async () => {
      const { remote, fake } = setup();
      await remote.leaveRoom(ROOM);
      expect(fake.sent.some((xml) => xml.includes("<publish "))).toBe(false);
    });
  });

  test("an outage after the home server stopped federating is not rejoined", async () => {
    await withXml(async () => {
      const { remote, fake, timer, federation } = setup();
      await remote.joinRoom(ROOM);
      federation.value = false;
      fake.depart(ROOM, [110, 333]);
      for (const entry of timer.pending.splice(0)) entry.callback();
      await settle();
      expect(fake.joined).toEqual([ROOM]);
    });
  });
});

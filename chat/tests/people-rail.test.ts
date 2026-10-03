import { afterEach, describe, expect, test } from "bun:test";
import { ref } from "vue";
import {
  buildMemberCards,
  buildPeopleRail,
  filterPeople,
  isAroundPresence,
  presenceFromShow,
  splitKnownPeople,
  usePeopleRail,
  type PeopleRailSources,
} from "../src/shell/controllers/use-people-rail";
import type { DmConversation, RosterContact } from "../src/lib/xmpp/types";
import type { MemberSummary, RoomBotSummary } from "../src/lib/chat-types";
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";
import { renderVueComponent } from "./helpers/render-vue-sfc";

const ROOM = "general@conference.example.com";

function contact(jid: string, presenceShow?: RosterContact["presenceShow"], name?: string): RosterContact {
  return { jid, username: jid.split("@")[0] ?? jid, name, subscription: "both", groups: [], presenceShow };
}

function conversation(peerJid: string, presenceShow?: DmConversation["presenceShow"], extra: Partial<DmConversation> = {}): DmConversation {
  return { peerJid, peerUsername: peerJid.split("@")[0] ?? peerJid, unreadCount: 0, presenceShow, ...extra };
}

function member(jid: string, affiliation: MemberSummary["affiliation"] = "member"): MemberSummary {
  return { jid, username: jid.split("@")[0] ?? jid, affiliation, joined_at: "" };
}

function sources(overrides: Partial<PeopleRailSources> = {}): PeopleRailSources {
  return {
    selfJid: "alice@example.com",
    roomPresence: {},
    authorJidByNick: {},
    members: [],
    roomBots: [],
    activeRoomJid: null,
    callParticipants: {},
    callParticipantOwners: {},
    ownCallRoomJid: null,
    speakingJids: new Set(),
    dmCallActivities: {},
    contacts: [],
    conversations: [],
    peerInCall: () => false,
    ...overrides,
  };
}

describe("buildPeopleRail", () => {
  test("maps the active room's call participants through authorJidByNick and marks speakers", () => {
    const groups = buildPeopleRail(sources({
      activeRoomJid: ROOM,
      roomPresence: { bob: "online", carol: "away", alice: "online" },
      authorJidByNick: { bob: "bob@example.com", carol: "carol@example.com", alice: "alice@example.com" },
      callParticipants: { [ROOM]: ["carol", "bob", "alice"] },
      speakingJids: new Set(["carol@example.com"]),
    }));

    expect(groups.huddle.map((p) => [p.jid, p.status, p.statusText])).toEqual([
      ["carol@example.com", "speaking", "speaking"],
      ["bob@example.com", "in-huddle", "in the huddle"],
    ]);
    expect(groups.huddle.every((p) => p.inCall)).toBe(true);
    // Self never appears, and huddle members are not repeated in the room group.
    expect(groups.room).toEqual([]);
  });

  test("uses Muji owner realJid for the call we are in when it is not the focused room", () => {
    const groups = buildPeopleRail(sources({
      ownCallRoomJid: "pairing@conference.example.com",
      callParticipants: { "pairing@conference.example.com": ["dave", "ghost"] },
      callParticipantOwners: { "pairing@conference.example.com": [{ nick: "dave", realJid: "dave@example.com/phone" }] },
    }));

    // `ghost` has no real JID, so it cannot be keyed and is omitted rather than invented.
    expect(groups.huddle.map((p) => p.jid)).toEqual(["dave@example.com"]);
    expect(groups.huddle[0]?.name).toBe("dave");
  });

  test("lists accepted DM calls in the huddle group", () => {
    const groups = buildPeopleRail(sources({
      conversations: [conversation("erin@example.com", "available")],
      dmCallActivities: {
        "erin@example.com": {
          peerJid: "erin@example.com",
          sid: "sid-1",
          media: { audio: true, video: false },
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-09-26T10:00:00.000Z",
        },
        "frank@example.com": {
          peerJid: "frank@example.com",
          sid: "sid-2",
          media: { audio: true, video: false },
          state: "ringing",
          direction: "outgoing",
          updatedAt: "2026-09-26T10:00:00.000Z",
        },
      },
    }));

    expect(groups.huddle.map((p) => p.jid)).toEqual(["erin@example.com"]);
    expect(groups.around).toEqual([]);
  });

  test("room group lists online occupants with a known JID, sorted available > away > dnd", () => {
    const groups = buildPeopleRail(sources({
      activeRoomJid: ROOM,
      roomPresence: { zed: "dnd", bob: "away", amy: "online", gone: "offline", anon: "online" },
      authorJidByNick: { zed: "zed@example.com", bob: "bob@example.com", amy: "amy@example.com", gone: "gone@example.com" },
      peerInCall: (jid) => jid === "bob@example.com",
    }));

    expect(groups.room.map((p) => [p.name, p.status, p.inCall])).toEqual([
      ["amy", "available", false],
      ["bob", "away", true],
      ["zed", "dnd", false],
    ]);
  });

  test("merges roster contacts and DM peers by bare JID and splits around from away/offline", () => {
    const groups = buildPeopleRail(sources({
      contacts: [
        contact("bob@example.com", "available", "Bob B"),
        contact("carol@example.com", "away"),
        contact("dan@example.com", "dnd"),
        contact("eve@example.com"),
      ],
      conversations: [
        conversation("bob@example.com", "available"),
        conversation("fay@example.com", "xa"),
        conversation("gus@example.com", "offline"),
        conversation("room@conference.example.com/nick", "available", { mucPm: true }),
      ],
      peerInCall: (jid) => jid === "dan@example.com",
    }));

    expect(groups.around.map((p) => [p.jid, p.name, p.status, p.inCall])).toEqual([
      ["bob@example.com", "Bob B", "available", false],
      ["dan@example.com", "dan", "dnd", true],
    ]);
    expect(groups.awayAndOffline.map((p) => [p.jid, p.status, p.presence])).toEqual([
      ["carol@example.com", "away", "away"],
      ["fay@example.com", "away", "away"],
      ["eve@example.com", "offline", undefined],
      ["gus@example.com", "offline", "offline"],
    ]);
  });

  test("a person appears once even when in every source", () => {
    const groups = buildPeopleRail(sources({
      activeRoomJid: ROOM,
      roomPresence: { bob: "online" },
      authorJidByNick: { bob: "bob@example.com" },
      callParticipants: { [ROOM]: ["bob"] },
      contacts: [contact("bob@example.com", "available")],
      conversations: [conversation("BOB@example.com", "available")],
    }));

    expect(groups.huddle.map((p) => p.jid)).toEqual(["bob@example.com"]);
    expect(groups.room).toEqual([]);
    expect(groups.around).toEqual([]);
    expect(groups.awayAndOffline).toEqual([]);
  });
});

describe("presenceFromShow / filterPeople", () => {
  test("folds 1:1 presence onto the occupant enum", () => {
    expect(presenceFromShow("available")).toBe("online");
    expect(presenceFromShow("xa")).toBe("away");
    expect(presenceFromShow("dnd")).toBe("dnd");
    expect(presenceFromShow("offline")).toBe("offline");
    expect(presenceFromShow(undefined)).toBeUndefined();
  });

  test("filters by name or JID, case-insensitively", () => {
    const groups = buildPeopleRail(sources({
      contacts: [contact("bob@example.com", "available", "Bob Builder"), contact("carol@example.com", "available")],
    }));
    expect(filterPeople(groups.around, "BUILD").map((p) => p.jid)).toEqual(["bob@example.com"]);
    expect(filterPeople(groups.around, "carol@").map((p) => p.jid)).toEqual(["carol@example.com"]);
    expect(filterPeople(groups.around, "  ").length).toBe(2);
  });
});

describe("buildMemberCards", () => {
  const base = {
    roomBots: [],
    roomPresence: {},
    authorJidByNick: {},
    contacts: [],
    conversations: [],
    huddleJids: new Set<string>(),
    speakingJids: new Set<string>(),
    peerInCall: () => false,
  };

  test("room cards carry affiliation and occupant presence through the reverse nick map", () => {
    const cards = buildMemberCards({
      ...base,
      roomActive: true,
      members: [member("owner@example.com", "owner"), member("bob@example.com"), member("zoe@example.com", "admin")],
      roomPresence: { bob: "online", zoe: "dnd" },
      authorJidByNick: { bob: "bob@example.com", zoe: "zoe@example.com" },
      huddleJids: new Set(["owner@example.com"]),
    });

    expect(cards.map((c) => [c.name, c.affiliation, c.status, c.presence, c.inCall])).toEqual([
      ["owner", "owner", "in-huddle", undefined, true],
      ["bob", "member", "available", "online", false],
      ["zoe", "admin", "dnd", "dnd", false],
    ]);
  });

  test("without a room the roster is the directory and affiliation is unknown", () => {
    const cards = buildMemberCards({
      ...base,
      roomActive: false,
      members: [member("ignored@example.com")],
      contacts: [contact("bob@example.com", "away", "Bobby")],
      conversations: [conversation("amy@example.com", "available")],
      speakingJids: new Set(["amy@example.com"]),
    });

    expect(cards.map((c) => [c.jid, c.name, c.affiliation, c.status])).toEqual([
      ["amy@example.com", "amy", null, "speaking"],
      ["bob@example.com", "Bobby", null, "away"],
    ]);
  });
});

describe("one rule for around", () => {
  test("isAroundPresence: available or dnd, never away, offline or unknown", () => {
    expect(isAroundPresence("online")).toBe(true);
    expect(isAroundPresence("dnd")).toBe(true);
    expect(isAroundPresence("away")).toBe(false);
    expect(isAroundPresence("offline")).toBe(false);
    expect(isAroundPresence(undefined)).toBe(false);
    // xa maps to away, so an extended-away contact is not around either.
    expect(isAroundPresence(presenceFromShow("xa"))).toBe(false);
  });

  test("splitKnownPeople merges roster and DM peers by bare JID with the rail's rule", () => {
    const { around, awayAndOffline } = splitKnownPeople(
      [
        contact("bob@example.com", "available", "Bob B"),
        contact("carol@example.com", "away"),
        contact("dan@example.com", "dnd"),
        contact("eve@example.com"),
      ],
      [
        conversation("BOB@example.com/phone", "dnd"),
        conversation("amy@example.com", "available"),
        conversation("fay@example.com", "xa"),
        conversation("room@conference.example.com/nick", "available", { mucPm: true }),
      ],
    );

    // A DM peer who is not on the roster counts; the conversation's
    // presence wins over the roster's, the roster name wins.
    expect(around.map((p) => [p.jid, p.name, p.presence, p.presenceShow])).toEqual([
      ["amy@example.com", "amy", "online", "available"],
      ["bob@example.com", "Bob B", "dnd", "dnd"],
      ["dan@example.com", "dan", "dnd", "dnd"],
    ]);
    expect(awayAndOffline.map((p) => [p.jid, p.presence])).toEqual([
      ["carol@example.com", "away"],
      ["fay@example.com", "away"],
      ["eve@example.com", undefined],
    ]);
    // The rail groups the same people the same way.
    const rail = buildPeopleRail(sources({
      contacts: [contact("bob@example.com", "available", "Bob B"), contact("carol@example.com", "away")],
      conversations: [conversation("amy@example.com", "available")],
    }));
    expect(rail.around.map((p) => p.jid)).toEqual(around.map((p) => p.jid).filter((jid) => jid !== "dan@example.com"));
    expect(rail.awayAndOffline.map((p) => p.jid)).toEqual(["carol@example.com"]);
  });
});

describe("usePeopleRail", () => {
  test("recomputes when a source ref changes", () => {
    const contacts = ref<RosterContact[]>([contact("bob@example.com", "available")]);
    const groups = usePeopleRail({
      selfJid: ref<string | null>("alice@example.com"),
      roomPresence: ref({}),
      authorJidByNick: ref({}),
      members: ref<MemberSummary[]>([]),
      roomBots: ref<RoomBotSummary[]>([]),
      activeRoomJid: ref<string | null>(null),
      callParticipants: ref({}),
      callParticipantOwners: ref({}),
      ownCallRoomJid: ref<string | null>(null),
      speakingJids: ref<ReadonlySet<string>>(new Set()),
      dmCallActivities: ref({}),
      contacts,
      conversations: ref<DmConversation[]>([]),
      peerInCall: () => false,
    });

    expect(groups.value.around.map((p) => p.jid)).toEqual(["bob@example.com"]);
    contacts.value = [...contacts.value, contact("amy@example.com", "available")];
    expect(groups.value.around.map((p) => p.jid)).toEqual(["amy@example.com", "bob@example.com"]);
  });
});

describe("bots take no DMs and have no presence, so they are listed apart from people", () => {
  afterEach(() => occupantJidDirectory.clear());
  const BOT = "helper@extensions.example.com";

  test("the rail lists a known bot once, in its own group with no status, outside every presence bucket", () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    const groups = buildPeopleRail(sources({
      activeRoomJid: ROOM,
      roomPresence: { helper: "online", bob: "online" },
      authorJidByNick: { helper: BOT, bob: "bob@example.com" },
      contacts: [contact(BOT, "available", "Helper"), contact("carol@example.com", "available")],
      conversations: [conversation(BOT, "available")],
    }));
    expect(groups.room.map((p) => p.jid)).toEqual(["bob@example.com"]);
    expect(groups.around.map((p) => p.jid)).toEqual(["carol@example.com"]);
    expect(groups.awayAndOffline).toEqual([]);
    expect(groups.bots).toEqual([
      { jid: BOT, name: "Helper", presence: undefined, status: "offline", statusText: null, inCall: false },
    ]);
  });

  test("a bot stays listed whether or not its occupant presence says it is in the room", () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    for (const presence of ["online", "away", "offline"] as const) {
      const groups = buildPeopleRail(sources({
        activeRoomJid: ROOM,
        roomPresence: { helper: presence, gone: "offline" },
        authorJidByNick: { helper: BOT, gone: "gone@example.com" },
      }));
      expect(groups.bots.map((p) => [p.jid, p.name])).toEqual([[BOT, "helper"]]);
      expect(groups.room).toEqual([]);
    }
  });

  test("the dashboard's around-now list leaves out a known bot", () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    const known = splitKnownPeople(
      [contact(BOT, "available"), contact("carol@example.com", "available")],
      [conversation(BOT, "available")],
    );
    expect(known.around.map((p) => p.jid)).toEqual(["carol@example.com"]);
    expect(known.awayAndOffline).toEqual([]);
  });

  test("member cards list the room's declared bots apart from its members, with no status", () => {
    const cards = buildMemberCards({
      roomActive: true,
      members: [member("bob@example.com")],
      roomBots: [{ jid: BOT, name: "Helper" }],
      roomPresence: { helper: "online", bob: "online" },
      authorJidByNick: { helper: BOT, bob: "bob@example.com" },
      contacts: [],
      conversations: [],
      huddleJids: new Set([BOT]),
      speakingJids: new Set(),
      peerInCall: () => false,
    });

    expect(cards.map((c) => [c.jid, c.name, c.bot, c.status, c.presence, c.statusText, c.inCall, c.affiliation])).toEqual([
      ["bob@example.com", "bob", false, "available", "online", "available", false, "member"],
      [BOT, "Helper", true, "offline", undefined, null, false, null],
    ]);
  });

  test("the rail renders the Bots group with no Message action and no status dot", async () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    const groups = buildPeopleRail(sources({
      activeRoomJid: ROOM,
      roomPresence: { helper: "online", bob: "online" },
      authorJidByNick: { helper: BOT, bob: "bob@example.com" },
    }));
    const html = (people: typeof groups.bots, bots: boolean) =>
      renderVueComponent(
        "../src/components/community/PeopleRailGroup.vue",
        { title: bots ? "Bots" : "In this room", count: people.length, people, bots },
        import.meta.url,
      );

    const botHtml = await html(groups.bots, true);
    expect(botHtml).toContain("Bots · 1");
    expect(botHtml).toContain("helper");
    expect(botHtml).not.toContain("<button");
    expect(botHtml).not.toContain("Message");
    expect(botHtml).not.toContain("data-show");

    // The same markup for a person is a Message button with a dot.
    const personHtml = await html(groups.room, false);
    expect(personHtml).toContain('aria-label="Message bob, available"');
    expect(personHtml).toContain('data-show="available"');
  });

  test("an avatar never shows a status for a known bot, even when handed one", async () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    const avatar = (jid: string) =>
      renderVueComponent(
        "../src/components/ui/UserAvatar.vue",
        { name: "x", jid, presence: "online", lastSeen: Date.now() },
        import.meta.url,
      );

    expect(await avatar(`${BOT}/bot`)).not.toContain("data-show");
    expect(await avatar("bob@example.com")).toContain('data-show="available"');
  });
});

describe("people rail after a restart", () => {
  afterEach(() => occupantJidDirectory.clear());
  const BOT = "helper@extensions.example.com";

  test("a declared room bot with no presence at all is listed under Bots by name", () => {
    const groups = buildPeopleRail(sources({
      activeRoomJid: ROOM,
      members: [member("bob@example.com")],
      roomBots: [{ jid: BOT, name: "Helper" }],
    }));
    expect(groups.bots).toEqual([
      { jid: BOT, name: "Helper", presence: undefined, status: "offline", statusText: null, inCall: false },
    ]);
    expect(groups.room).toEqual([]);
  });

  test("a bot that is both declared and present is listed once", () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    const groups = buildPeopleRail(sources({
      activeRoomJid: ROOM,
      roomPresence: { helper: "online" },
      authorJidByNick: { helper: BOT },
      roomBots: [{ jid: BOT, name: "Helper" }, { jid: BOT.toUpperCase(), name: "Helper" }],
    }));
    expect(groups.bots.map((bot) => [bot.jid, bot.name])).toEqual([[BOT, "Helper"]]);
    expect(groups.room).toEqual([]);
  });

  test("a hatted occupant the declared list lacks is still a bot, not a person", () => {
    occupantJidDirectory.record(ROOM, "newbie", "newbie@extensions.example.com/bot", true);
    const groups = buildPeopleRail(sources({
      activeRoomJid: ROOM,
      roomPresence: { newbie: "online" },
      authorJidByNick: { newbie: "newbie@extensions.example.com" },
    }));
    expect(groups.bots.map((bot) => bot.jid)).toEqual(["newbie@extensions.example.com"]);
    expect(groups.room).toEqual([]);
  });

  test("declared bots belong to the focused room: with no room, none are listed", () => {
    expect(buildPeopleRail(sources({ roomBots: [{ jid: BOT, name: "Helper" }] })).bots).toEqual([]);
  });
});

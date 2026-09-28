import { describe, expect, test } from "bun:test";
import type { ChannelSummary, GroupDmSummary, SpaceSummary } from "../src/lib/chat-types";
import type { DmConversation, RosterContact } from "../src/lib/xmpp/types";
import {
  buildQuickSwitcherEntries,
  moveQuickSwitcherHighlight,
  rankQuickSwitcherEntries,
  resolveQuickSwitcherHighlight,
  type QuickSwitcherEntry,
} from "../src/lib/quick-switcher";

const penguins: SpaceSummary = { id: "s1", name: "Penguins" };
const puffins: SpaceSummary = { id: "s2", name: "Puffins" };

const general: ChannelSummary = { id: "c1", name: "general", spaceId: "s1", jid: "general@muc.example.com", position: 0 };
const cafe: ChannelSummary = { id: "c2", name: "Café Talk", spaceId: "s1", position: 1, channel_type: "forum" };
const nests: ChannelSummary = { id: "c3", name: "nests", spaceId: "s2", position: 0 };
const loose: ChannelSummary = { id: "c4", name: "lobby", position: 0 };

const group: GroupDmSummary = { id: "g1", roomJid: "g1@groups.example.com", name: "Weekend plans", unreadCount: 2 };

function conversation(peerJid: string, peerUsername: string, extra: Partial<DmConversation> = {}): DmConversation {
  return { peerJid, peerUsername, unreadCount: 0, ...extra };
}

function contact(jid: string, username: string, name?: string): RosterContact {
  return { jid, username, name, subscription: "both", groups: [] };
}

function build(overrides: Partial<Parameters<typeof buildQuickSwitcherEntries>[0]> = {}) {
  return buildQuickSwitcherEntries({
    spaces: [penguins, puffins],
    channels: [general, cafe, nests, loose],
    channelUnread: {},
    groupDms: [],
    conversations: [],
    contacts: [],
    ...overrides,
  });
}

function titles(entries: readonly QuickSwitcherEntry[]): string[] {
  return entries.map((entry) => entry.title);
}

describe("buildQuickSwitcherEntries", () => {
  test("lists rooms by waddle, then group chats, DMs, contacts and pages", () => {
    const entries = build({
      groupDms: [group],
      conversations: [conversation("bob@example.com", "bob")],
      contacts: [contact("carol@example.com", "carol")],
    });
    expect(titles(entries)).toEqual([
      "general", "Café Talk", "nests", "lobby",
      "Weekend plans",
      "bob",
      "carol",
      "Home", "Rooms", "Discussions", "Unread", "Members", "Feed", "Events", "Settings",
    ]);
    expect(entries[0]?.target).toEqual({ kind: "channel", channelId: "c1", roomJid: "general@muc.example.com" });
    expect(entries[1]?.forum).toBe(true);
    expect(entries[4]).toMatchObject({ subtitle: "Group chat", unread: 2, target: { kind: "groupDm", roomJid: "g1@groups.example.com" } });
    expect(entries[5]).toMatchObject({ subtitle: "bob@example.com", target: { kind: "dm", peerJid: "bob@example.com" } });
    expect(entries.at(-1)?.target).toEqual({ kind: "page", page: "settings" });
    expect(new Set(entries.map((entry) => entry.id)).size).toBe(entries.length);
  });

  test("names the waddle only when there is more than one, never the standalone group", () => {
    const multi = build();
    expect(multi.map((entry) => entry.subtitle).slice(0, 4)).toEqual(["Penguins", "Penguins", "Puffins", undefined]);
    const single = build({ spaces: [penguins], channels: [general, loose] });
    expect(single.slice(0, 2).map((entry) => entry.subtitle)).toEqual([undefined, undefined]);
  });

  test("skips group DMs in the room list and merges unread and mention counts", () => {
    const entries = build({
      channels: [general, { id: "g1", name: "Weekend plans", isGroupDm: true }],
      channelUnread: { c1: { unread: 3, mentions: 1 } },
    });
    expect(entries.filter((entry) => entry.target.kind === "channel").map((entry) => entry.title)).toEqual(["general"]);
    expect(entries[0]).toMatchObject({ unread: 3, mentionsMe: true });
  });

  test("lists a contact once, matching an existing DM case-insensitively", () => {
    const entries = build({
      channels: [],
      conversations: [conversation("Bob@Example.com", "bob")],
      contacts: [contact("bob@example.com", "bob", "Bob B."), contact("carol@example.com", "carol")],
    });
    const people = entries.filter((entry) => entry.target.kind === "dm");
    expect(people.map((entry) => entry.id)).toEqual(["dm:Bob@Example.com", "contact:carol@example.com"]);
  });

  test("leaves out MUC private chats so a room nick cannot pose as an account (#1256)", () => {
    const entries = build({
      channels: [],
      conversations: [
        conversation("lobby@muc.example.com/alice@example.com", "alice@example.com (lobby)", { mucPm: true, unreadCount: 1 }),
        conversation("alice@example.com", "alice"),
      ],
      contacts: [contact("alice@example.com", "alice"), contact("carol@example.com", "carol")],
    });
    expect(entries.filter((entry) => entry.target.kind === "dm").map((entry) => entry.id)).toEqual([
      "dm:alice@example.com",
      "contact:carol@example.com",
    ]);
    for (const query of ["alice@example.com", "alice"]) {
      expect(rankQuickSwitcherEntries(entries, query)[0]?.target).toEqual({ kind: "dm", peerJid: "alice@example.com" });
    }
  });

  test("names contacts by roster name, then username, sorted by name", () => {
    const entries = build({
      channels: [],
      contacts: [contact("z@example.com", "zed", "Alice"), contact("y@example.com", "yan")],
    });
    expect(titles(entries.filter((entry) => entry.id.startsWith("contact:")))).toEqual(["Alice", "yan"]);
  });
});

describe("rankQuickSwitcherEntries", () => {
  const entry = (title: string, extra: Partial<QuickSwitcherEntry> = {}): QuickSwitcherEntry => ({
    id: title,
    title,
    unread: 0,
    mentionsMe: false,
    target: { kind: "page", page: "home" },
    ...extra,
  });

  test("orders title prefix, word prefix, title substring, then subtitle substring", () => {
    const entries = [
      entry("design-review", { subtitle: "Team" }),
      entry("art team"),
      entry("teammates"),
      entry("steam"),
      entry("unrelated"),
    ];
    expect(titles(rankQuickSwitcherEntries(entries, "team"))).toEqual(["teammates", "art team", "steam", "design-review"]);
  });

  test("breaks ties by mentions, then unread, then list order", () => {
    const entries = [entry("a1"), entry("a2", { unread: 4 }), entry("a3", { mentionsMe: true }), entry("a4")];
    expect(titles(rankQuickSwitcherEntries(entries, ""))).toEqual(["a3", "a2", "a1", "a4"]);
    expect(titles(rankQuickSwitcherEntries(entries, "a"))).toEqual(["a3", "a2", "a1", "a4"]);
  });

  test("ignores case, accents, surrounding space and a leading # or @", () => {
    const entries = [entry("Café Talk"), entry("general"), entry("bob")];
    expect(titles(rankQuickSwitcherEntries(entries, "  CAFE "))).toEqual(["Café Talk"]);
    expect(titles(rankQuickSwitcherEntries(entries, "#gen"))).toEqual(["general"]);
    expect(titles(rankQuickSwitcherEntries(entries, "@bo"))).toEqual(["bob"]);
    expect(titles(rankQuickSwitcherEntries(entries, "#"))).toEqual(["Café Talk", "general", "bob"]);
  });

  test("caps the list", () => {
    const entries = Array.from({ length: 60 }, (_, index) => entry(`room ${index}`));
    expect(rankQuickSwitcherEntries(entries, "")).toHaveLength(50);
    expect(rankQuickSwitcherEntries(entries, "room", 3)).toHaveLength(3);
  });
});

describe("quick switcher highlight", () => {
  const ids = ["a", "b", "c"];

  test("keeps a listed highlight, else falls back to the first row", () => {
    expect(resolveQuickSwitcherHighlight("b", ids)).toBe("b");
    expect(resolveQuickSwitcherHighlight("gone", ids)).toBe("a");
    expect(resolveQuickSwitcherHighlight(null, ids)).toBe("a");
    expect(resolveQuickSwitcherHighlight("a", [])).toBeNull();
  });

  test("moves and wraps at either end", () => {
    expect(moveQuickSwitcherHighlight("a", 1, ids)).toBe("b");
    expect(moveQuickSwitcherHighlight("c", 1, ids)).toBe("a");
    expect(moveQuickSwitcherHighlight("a", -1, ids)).toBe("c");
    expect(moveQuickSwitcherHighlight(null, 1, ids)).toBe("a");
    expect(moveQuickSwitcherHighlight(null, -1, ids)).toBe("c");
    expect(moveQuickSwitcherHighlight("a", 1, [])).toBeNull();
  });
});

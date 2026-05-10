import { describe, expect, test } from "bun:test";
import {
  avatarLookupCandidates,
  messageMentionsBareJid,
  mentionAutocompleteCandidates,
  mentionAutocompleteNames,
  mentionMatchesBareJid,
  mentionMatchesUsername,
  mergeMentionMembers,
  resolveMentionUri,
} from "../src/lib/mentions";

describe("mention helpers", () => {
  test("matches mentions against usernames case-insensitively", () => {
    expect(mentionMatchesUsername("xmpp:Rawkode@waddle.social", "rawkode")).toBe(true);
    expect(mentionMatchesUsername("ICEPUMA", "icepuma")).toBe(true);
    expect(mentionMatchesUsername("randax@waddle.social", "rawkode")).toBe(false);
  });

  test("matches personal mention references by exact bare JID", () => {
    expect(mentionMatchesBareJid("xmpp:rawkode@waddle.social", "rawkode@waddle.social/desktop")).toBe(true);
    expect(mentionMatchesBareJid("rawkode@waddle.social?message", "rawkode@waddle.social")).toBe(true);
    expect(mentionMatchesBareJid("xmpp:rawkode@other.example", "rawkode@waddle.social")).toBe(false);
    expect(mentionMatchesBareJid("rawkode", "rawkode@waddle.social")).toBe(false);
  });

  test("detects rendered message mentions by exact bare JID", () => {
    expect(messageMentionsBareJid(
      { mentions: ["xmpp:alice@example.com"] },
      "alice@example.com/desktop",
    )).toBe(true);
    expect(messageMentionsBareJid(
      { mentions: ["xmpp:alice@other.example"] },
      "alice@example.com/desktop",
    )).toBe(false);
    expect(messageMentionsBareJid(
      { mentions: ["alice"] },
      "alice@example.com/desktop",
    )).toBe(false);
    expect(messageMentionsBareJid(
      { broadcastMention: "everyone" },
      null,
    )).toBe(true);
  });

  test("resolves room nick mentions through known member bare JIDs", () => {
    expect(resolveMentionUri("Bob", { bob: "bob@localhost" })).toBe("xmpp:bob@localhost");
    expect(resolveMentionUri("rawkode", { Rawkode: "Rawkode@waddle.social" })).toBe("xmpp:rawkode@waddle.social");
    expect(resolveMentionUri("icepuma")).toBe("xmpp:icepuma");
  });

  test("keeps broadcast mentions ahead of merged member candidates", () => {
    expect(mentionAutocompleteNames(["alice", "Here", "bob", "everyone"])).toEqual([
      "everyone",
      "here",
      "alice",
      "bob",
    ]);
  });

  test("merges live presence occupants with bare JIDs into mention members", () => {
    const merged = mergeMentionMembers({
      members: [{
        jid: "alice@example.com",
        username: "alice",
        avatar_url: null,
        role: "owner",
        joined_at: "",
      }],
      roomPresence: {
        alice: "online",
        bob: "away",
        carol: "offline",
      },
      memberJidsByNick: {
        Bob: "bob@example.com",
      },
    });

    expect(merged.members.map((member) => member.username)).toEqual(["alice", "bob"]);
    expect(merged.authorJidByNick).toEqual({
      alice: "alice@example.com",
      bob: "bob@example.com",
    });
    expect(merged.diagnostics).toEqual([
      "Presence invariant violated: missing bare occupant JIDs for alice.",
    ]);
  });

  test("autocomplete includes merged presence occupants alongside broadcast mentions", () => {
    const merged = mergeMentionMembers({
      members: [{ jid: "alice@example.com", username: "alice", avatar_url: null, role: "owner", joined_at: "" }],
      roomPresence: { alice: "online", bob: "online" },
      memberJidsByNick: { Bob: "bob@example.com" },
    });

    const names = mentionAutocompleteNames(merged.members.map((m) => m.username));

    // Broadcasts lead, then member names in original order
    expect(names).toEqual(["everyone", "here", "alice", "bob"]);
  });

  test("displayed member counts include affiliation members and live occupants", () => {
    const merged = mergeMentionMembers({
      members: [{ jid: "alice@example.com", username: "alice", avatar_url: null, role: "owner", joined_at: "" }],
      roomPresence: { alice: "online", bob: "online", carol: "offline" },
      memberJidsByNick: { Bob: "bob@example.com" },
    });

    expect(merged.members.map((member) => member.username)).toEqual(["alice", "bob"]);
    expect(merged.members).toHaveLength(2);
  });

  test("autocomplete candidates include registered offline members once", () => {
    const merged = mergeMentionMembers({
      members: [
        { jid: "alice@example.com", username: "alice", avatar_url: "https://example.com/alice.png", role: "owner", joined_at: "" },
        { jid: "bob@example.com", username: "bob", avatar_url: null, role: "member", joined_at: "" },
      ],
      roomPresence: {},
      memberJidsByNick: {},
    });

    const candidates = mentionAutocompleteCandidates(merged.members);

    expect(candidates.map((candidate) => candidate.username)).toEqual(["everyone", "here", "alice", "bob"]);
    expect(candidates.filter((candidate) => candidate.username === "everyone")).toHaveLength(1);
    expect(candidates.find((candidate) => candidate.username === "alice")).toMatchObject({
      jid: "alice@example.com",
      avatar_url: "https://example.com/alice.png",
      kind: "member",
    });
    expect(resolveMentionUri("bob", merged.authorJidByNick)).toBe("xmpp:bob@example.com");
  });

  test("autocomplete candidates exclude non-participating affiliations and broadcast collisions", () => {
    const candidates = mentionAutocompleteCandidates([
      { jid: "here@example.com", username: "here", avatar_url: null, role: "member", joined_at: "" },
      { jid: "mallory@example.com", username: "mallory", avatar_url: null, role: "outcast", joined_at: "" },
      { jid: "nobody@example.com", username: "nobody", avatar_url: null, role: "none", joined_at: "" },
      { jid: "alice@example.com", username: "alice", avatar_url: null, role: "admin", joined_at: "" },
    ]);

    expect(candidates.map((candidate) => candidate.username)).toEqual(["everyone", "here", "alice"]);
  });

  test("resolveMentionUri resolves merged presence occupants via their bare JIDs", () => {
    const merged = mergeMentionMembers({
      members: [],
      roomPresence: { bob: "online" },
      memberJidsByNick: { Bob: "bob@example.com" },
    });

    // bob was added via presence merge; his nick should resolve to his bare JID
    expect(resolveMentionUri("bob", merged.authorJidByNick)).toBe("xmpp:bob@example.com");
  });

  test("resolveMentionUri falls back to nick-as-JID when merged occupant has no bare JID", () => {
    // anonymous room: memberJidsByNick is empty, so merged.authorJidByNick has no entry
    const merged = mergeMentionMembers({
      members: [],
      roomPresence: { carol: "online" },
      memberJidsByNick: {},
    });

    // carol has no bare JID; URI falls back to the nick itself
    expect(resolveMentionUri("carol", merged.authorJidByNick)).toBe("xmpp:carol");
  });

  test("surfaces anonymous-room diagnostics when live occupants have no bare JIDs", () => {
    const merged = mergeMentionMembers({
      members: [],
      roomPresence: {
        alice: "online",
        bob: "dnd",
      },
      memberJidsByNick: {},
    });

    expect(merged.members).toEqual([]);
    expect(merged.diagnostics).toEqual([
      "Presence invariant violated: room appears anonymous or omitted bare occupant JIDs for alice, bob.",
    ]);
  });

  test("avatar lookup candidates include visible MAM authors missing from members", () => {
    const candidates = avatarLookupCandidates({
      members: [{ jid: "rawkode@waddle.social", username: "rawkode", avatar_url: null, role: "owner", joined_at: "" }],
      messages: [
        {
          author: "randax",
          authorJid: "chat@muc.waddle.social/randax",
          authorRealJid: "randax@waddle.social/laptop",
        },
        {
          author: "icepuma",
          authorJid: "chat@muc.waddle.social/icepuma",
        },
      ],
      authorJidByNick: {},
      selfDomain: "waddle.social",
    });

    expect(candidates.map((candidate) => candidate.jid)).toEqual([
      "rawkode@waddle.social",
      "randax@waddle.social",
      "icepuma@waddle.social",
    ]);
  });

  test("avatar lookup candidates prefer member and presence JIDs over inferred JIDs", () => {
    const candidates = avatarLookupCandidates({
      members: [],
      messages: [
        { author: "Randax", authorJid: "chat@muc.waddle.social/Randax" },
        { author: "icepuma", authorJid: "chat@muc.waddle.social/icepuma" },
      ],
      authorJidByNick: {
        randax: "randax@waddle.social",
        Icepuma: "icepuma@elsewhere.example",
      },
      selfDomain: "waddle.social",
    });

    expect(candidates.map((candidate) => candidate.jid)).toEqual([
      "randax@waddle.social",
      "icepuma@elsewhere.example",
    ]);
  });

  test("avatar lookup candidates do not use MUC occupant JIDs directly", () => {
    const candidates = avatarLookupCandidates({
      members: [],
      messages: [
        { author: "randax", authorJid: "chat@muc.waddle.social/randax" },
      ],
      authorJidByNick: {},
      selfDomain: "waddle.social",
    });

    expect(candidates).toEqual([
      { nick: "randax", jid: "randax@waddle.social", avatar_url: null },
    ]);
  });

  // RFC 363 PR 6: regression — passing DM messages through the
  // channel-only `authorJidByNick` map can resolve a foreign-domain
  // DM peer to a same-nick channel member's JID. Callers must split
  // DM resolution into a separate call with empty channel context.
  test("avatar lookup candidates from empty channel context resolve DM peers via message authorJid", () => {
    const candidates = avatarLookupCandidates({
      members: [],
      messages: [
        { author: "alice", authorJid: "alice@other.example/desktop" },
      ],
      authorJidByNick: {},
      selfDomain: "waddle.social",
    });

    expect(candidates).toEqual([
      { nick: "alice", jid: "alice@other.example", avatar_url: null },
    ]);
  });

  test("avatar lookup candidates with channel context misresolve DM-shaped messages — split required", () => {
    // Demonstrates the bug being fixed: passing DM messages into a
    // channel-context call resolves the DM author through
    // `mappedJidByNick`, ignoring the DM's own `authorJid`. The
    // production fix at chat/src/shell/chat-app-controller.ts splits
    // DM messages into their own call with empty members /
    // authorJidByNick to avoid this collision.
    const candidates = avatarLookupCandidates({
      members: [{ jid: "alice@waddle.social", username: "alice", avatar_url: null, role: "member", joined_at: "" }],
      messages: [
        { author: "alice", authorJid: "alice@other.example/desktop" },
      ],
      authorJidByNick: { alice: "alice@waddle.social" },
      selfDomain: "waddle.social",
    });

    // alice resolves to channel member, NOT to the DM `authorJid`.
    // This is why splitting calls per-context is necessary.
    expect(candidates.map((c) => c.jid)).toEqual(["alice@waddle.social"]);
    expect(candidates.map((c) => c.jid)).not.toContain("alice@other.example");
  });
});

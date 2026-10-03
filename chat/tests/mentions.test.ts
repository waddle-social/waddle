import { describe, expect, test } from "bun:test";
import {
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
        affiliation: "owner",
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

  test("a posting bot wearing the Bot hat resolves its nick but is never a member", () => {
    const merged = mergeMentionMembers({
      members: [],
      roomHats: { Helper: [{ uri: "urn:waddle:hats:bot", title: "Bot" }] },
      roomPresence: { Helper: "online", bob: "online" },
      memberJidsByNick: { Helper: "helper@extensions.example.com", bob: "bob@example.com" },
    });

    expect(merged.members.map((member) => member.username)).toEqual(["bob"]);
    expect(merged.authorJidByNick.Helper).toBe("helper@extensions.example.com");
  });

  test("autocomplete includes merged presence occupants alongside broadcast mentions", () => {
    const merged = mergeMentionMembers({
      members: [{ jid: "alice@example.com", username: "alice", affiliation: "owner", joined_at: "" }],
      roomPresence: { alice: "online", bob: "online" },
      memberJidsByNick: { Bob: "bob@example.com" },
    });

    const names = mentionAutocompleteNames(merged.members.map((m) => m.username));

    // Broadcasts lead, then member names in original order
    expect(names).toEqual(["everyone", "here", "alice", "bob"]);
  });

  test("displayed member counts include affiliation members and live occupants", () => {
    const merged = mergeMentionMembers({
      members: [{ jid: "alice@example.com", username: "alice", affiliation: "owner", joined_at: "" }],
      roomPresence: { alice: "online", bob: "online", carol: "offline" },
      memberJidsByNick: { Bob: "bob@example.com" },
    });

    expect(merged.members.map((member) => member.username)).toEqual(["alice", "bob"]);
    expect(merged.members).toHaveLength(2);
  });

  test("autocomplete candidates include registered offline members once", () => {
    const merged = mergeMentionMembers({
      members: [
        { jid: "alice@example.com", username: "alice", affiliation: "owner", joined_at: "" },
        { jid: "bob@example.com", username: "bob", affiliation: "member", joined_at: "" },
      ],
      roomPresence: {},
      memberJidsByNick: {},
    });

    const candidates = mentionAutocompleteCandidates(merged.members);

    expect(candidates.map((candidate) => candidate.username)).toEqual(["everyone", "here", "alice", "bob"]);
    expect(candidates.filter((candidate) => candidate.username === "everyone")).toHaveLength(1);
    expect(candidates.find((candidate) => candidate.username === "alice")).toMatchObject({
      jid: "alice@example.com",
      kind: "member",
    });
    expect(resolveMentionUri("bob", merged.authorJidByNick)).toBe("xmpp:bob@example.com");
  });

  test("autocomplete candidates exclude non-participating affiliations and broadcast collisions", () => {
    const candidates = mentionAutocompleteCandidates([
      { jid: "here@example.com", username: "here", affiliation: "member", joined_at: "" },
      { jid: "mallory@example.com", username: "mallory", affiliation: "outcast", joined_at: "" },
      { jid: "nobody@example.com", username: "nobody", affiliation: "none", joined_at: "" },
      { jid: "alice@example.com", username: "alice", affiliation: "admin", joined_at: "" },
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
});

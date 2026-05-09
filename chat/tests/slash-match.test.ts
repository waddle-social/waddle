import { describe, expect, test } from "bun:test";
import type { DiscoveredExtensionCommand } from "../src/lib/xmpp/extension-commands";
import { filterSlashCandidates, resolveSlashCommand } from "../src/lib/slash-match";

const ai: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:ai-chatbot",
  name: "Ask AI Chatbot",
  scope: "global",
  composerPrefix: "ai",
  inlineField: "prompt",
};

const poll: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:decision-polls",
  name: "Create Decision Poll",
  scope: "channel",
  composerPrefix: "poll",
};

const link: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:link-board",
  name: "Add Link",
  scope: "channel",
  composerPrefix: "link",
};

const noPrefix: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:ops-panic",
  name: "Panic Button",
  scope: "global",
};

describe("filterSlashCandidates", () => {
  test("returns nothing when commands list is empty", () => {
    expect(filterSlashCandidates("ai", [], { inMuc: true })).toEqual([]);
  });

  test("skips commands without a composerPrefix (opt-in only)", () => {
    expect(filterSlashCandidates("", [noPrefix, ai], { inMuc: true })).toEqual([ai]);
  });

  test("hides channel-scoped commands when not currently in a MUC", () => {
    expect(filterSlashCandidates("", [ai, poll, link], { inMuc: false })).toEqual([ai]);
  });

  test("includes channel-scoped commands when in a MUC", () => {
    expect(filterSlashCandidates("", [ai, poll, link], { inMuc: true })).toEqual([ai, poll, link]);
  });

  test("filters by startsWith on composerPrefix (case-insensitive)", () => {
    expect(filterSlashCandidates("a", [ai, poll, link], { inMuc: true })).toEqual([ai]);
    expect(filterSlashCandidates("AI", [ai, poll, link], { inMuc: true })).toEqual([ai]);
    expect(filterSlashCandidates("l", [ai, poll, link], { inMuc: true })).toEqual([link]);
  });

  test("returns empty when nothing starts with the prefix", () => {
    expect(filterSlashCandidates("z", [ai, poll, link], { inMuc: true })).toEqual([]);
  });

  test("empty prefix returns all eligible commands", () => {
    expect(filterSlashCandidates("", [ai, poll, link], { inMuc: true })).toEqual([ai, poll, link]);
  });
});

describe("resolveSlashCommand", () => {
  test("returns null when prefix is empty (ambiguous)", () => {
    expect(resolveSlashCommand("", [ai, poll, link], { inMuc: true })).toBeNull();
  });

  test("returns null when nothing exactly matches", () => {
    expect(resolveSlashCommand("xyz", [ai, poll, link], { inMuc: true })).toBeNull();
  });

  test("ignores partial matches", () => {
    expect(resolveSlashCommand("a", [ai, poll, link], { inMuc: true })).toBeNull();
  });

  test("returns the exact match (case-insensitive)", () => {
    expect(resolveSlashCommand("ai", [ai, poll, link], { inMuc: true })).toBe(ai);
    expect(resolveSlashCommand("AI", [ai, poll, link], { inMuc: true })).toBe(ai);
  });

  test("returns null for channel-scoped command outside a MUC", () => {
    expect(resolveSlashCommand("poll", [ai, poll, link], { inMuc: false })).toBeNull();
  });

  test("ignores commands without a composerPrefix", () => {
    expect(resolveSlashCommand("ops-panic", [noPrefix], { inMuc: true })).toBeNull();
  });

  test("returns null when two commands advertise the same composerPrefix", () => {
    const dupe: DiscoveredExtensionCommand = {
      serviceJid: "extensions.example.com",
      node: "urn:waddle:extension:1:other-ai",
      name: "Other AI",
      scope: "global",
      composerPrefix: "ai",
    };
    expect(resolveSlashCommand("ai", [ai, dupe], { inMuc: true })).toBeNull();
  });
});

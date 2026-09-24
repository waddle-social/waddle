import { describe, expect, test } from "bun:test";
import type { DiscoveredExtensionCommand } from "../src/lib/xmpp/extension-commands";
import {
  listSlashCandidates,
  resolveSlashTarget,
  slashCandidateName,
} from "../src/lib/slash-candidates";

function ext(composerPrefix: string, overrides: Partial<DiscoveredExtensionCommand> = {}): DiscoveredExtensionCommand {
  return {
    serviceJid: "extensions.example.com",
    node: `urn:waddle:extension:1:${composerPrefix}`,
    name: composerPrefix,
    scope: "global",
    composerPrefix,
    ...overrides,
  };
}

const poll = ext("poll", { scope: "channel" });
const meet = ext("meet");

describe("listSlashCandidates", () => {
  test("empty prefix lists every built-in, then eligible extensions", () => {
    const names = listSlashCandidates("", [poll, meet], { inMuc: true }).map(slashCandidateName);
    expect(names).toEqual(["me", "shrug", "giphy", "away", "active", "dnd", "poll", "meet"]);
  });

  test("built-ins come first and are listed even outside MUCs", () => {
    const candidates = listSlashCandidates("me", [poll, meet], { inMuc: false });
    expect(candidates.map((c) => c.kind)).toEqual(["builtin", "extension"]);
    expect(candidates.map(slashCandidateName)).toEqual(["me", "meet"]);
  });

  test("aliases match by prefix but the built-in is listed once under its name", () => {
    expect(listSlashCandidates("gi", [], { inMuc: true }).map(slashCandidateName)).toEqual(["giphy"]);
    expect(listSlashCandidates("GIF", [], { inMuc: true }).map(slashCandidateName)).toEqual(["giphy"]);
  });

  test("extensions sharing a built-in name or alias are hidden", () => {
    const names = listSlashCandidates("", [ext("shrug"), ext("gif"), poll], { inMuc: true }).map(slashCandidateName);
    expect(names).toEqual(["me", "shrug", "giphy", "away", "active", "dnd", "poll"]);
  });

  test("channel-scope filtering still applies to extensions", () => {
    expect(listSlashCandidates("po", [poll], { inMuc: false })).toEqual([]);
  });
});

describe("resolveSlashTarget", () => {
  test("a ready built-in resolves with its outcome", () => {
    expect(resolveSlashTarget("gif", "cats", [], { inMuc: false })).toMatchObject({
      kind: "builtin",
      command: { name: "giphy" },
      outcome: { kind: "open-gif-picker", query: "cats" },
    });
  });

  test("a built-in wins over an extension with the same prefix", () => {
    const shadow = ext("away");
    expect(resolveSlashTarget("away", "", [shadow], { inMuc: true })).toMatchObject({ kind: "builtin" });
  });

  test("a built-in missing its argument resolves to nothing (and does not fall back to an extension)", () => {
    expect(resolveSlashTarget("me", "", [ext("me")], { inMuc: true })).toBeNull();
  });

  test("extension commands still resolve", () => {
    expect(resolveSlashTarget("poll", "", [poll], { inMuc: true })).toEqual({ kind: "extension", command: poll });
    expect(resolveSlashTarget("poll", "", [poll], { inMuc: false })).toBeNull();
  });

  test("unknown prefixes resolve to nothing", () => {
    expect(resolveSlashTarget("xyz", "", [poll], { inMuc: true })).toBeNull();
  });
});

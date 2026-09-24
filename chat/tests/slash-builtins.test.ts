import { describe, expect, test } from "bun:test";
import {
  BUILTIN_SLASH_COMMANDS,
  findBuiltinSlash,
  resolveBuiltinSlash,
} from "../src/lib/slash-builtins";

describe("BUILTIN_SLASH_COMMANDS", () => {
  test("lists each built-in once with usage and description", () => {
    expect(BUILTIN_SLASH_COMMANDS.map((c) => c.name)).toEqual(["me", "shrug", "giphy", "away", "active", "dnd"]);
    for (const command of BUILTIN_SLASH_COMMANDS) {
      expect(command.usage.startsWith(`/${command.name}`)).toBe(true);
      expect(command.description.length).toBeGreaterThan(0);
    }
  });

  test("names and aliases are unique across the table", () => {
    const keywords = BUILTIN_SLASH_COMMANDS.flatMap((c) => [c.name, ...c.aliases]);
    expect(new Set(keywords).size).toBe(keywords.length);
  });
});

describe("findBuiltinSlash", () => {
  test("matches names and aliases case-insensitively", () => {
    expect(findBuiltinSlash("giphy")?.name).toBe("giphy");
    expect(findBuiltinSlash("GIF")?.name).toBe("giphy");
    expect(findBuiltinSlash("Me")?.name).toBe("me");
  });

  test("ignores empty, partial and unknown prefixes", () => {
    expect(findBuiltinSlash("")).toBeNull();
    expect(findBuiltinSlash("shr")).toBeNull();
    expect(findBuiltinSlash("poll")).toBeNull();
  });
});

describe("resolveBuiltinSlash", () => {
  test("returns null for non built-ins", () => {
    expect(resolveBuiltinSlash("poll", "x")).toBeNull();
  });

  test("/me sends only with a non-empty action", () => {
    expect(resolveBuiltinSlash("me", "waves")).toMatchObject({
      status: "ready",
      outcome: { kind: "send", rewrite: "me" },
    });
    expect(resolveBuiltinSlash("me", "")).toMatchObject({ status: "missing-argument", command: { name: "me" } });
    expect(resolveBuiltinSlash("me", "   ")).toMatchObject({ status: "missing-argument" });
  });

  test("/shrug sends with or without text", () => {
    expect(resolveBuiltinSlash("shrug", "")).toMatchObject({ status: "ready", outcome: { kind: "send", rewrite: "shrug" } });
    expect(resolveBuiltinSlash("SHRUG", "oh well")).toMatchObject({ status: "ready", outcome: { kind: "send", rewrite: "shrug" } });
  });

  test("/giphy and /gif open the picker with the trimmed query", () => {
    expect(resolveBuiltinSlash("giphy", " cats  ")).toMatchObject({ outcome: { kind: "open-gif-picker", query: "cats" } });
    expect(resolveBuiltinSlash("gif", "")).toMatchObject({ outcome: { kind: "open-gif-picker", query: "" } });
  });

  test("presence commands map to manual presence picks and ignore trailing text", () => {
    expect(resolveBuiltinSlash("away", "")).toMatchObject({ outcome: { kind: "set-presence", pick: "away" } });
    expect(resolveBuiltinSlash("active", "")).toMatchObject({ outcome: { kind: "set-presence", pick: "available" } });
    expect(resolveBuiltinSlash("dnd", "focus time")).toMatchObject({ outcome: { kind: "set-presence", pick: "dnd" } });
  });
});

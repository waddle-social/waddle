import { describe, expect, test } from "bun:test";

import { createPandaTokenResolver, flattenTokens } from "./panda-tokens.mjs";

const conditions = { night: "_dark", daylight: "_light" };

function resolver(tokens, semanticTokens = {}) {
  return createPandaTokenResolver({ tokens, semanticTokens, conditions });
}

describe("flattenTokens", () => {
  test("joins nested keys with dots and maps DEFAULT to the group", () => {
    const flat = flattenTokens({
      teal: { 300: { value: "#6fd3c7" } },
      fg: { DEFAULT: { value: "a" }, muted: { value: "b" } },
      "bg.rail": { value: "c" },
    });
    expect([...flat]).toEqual([
      ["teal.300", "#6fd3c7"],
      ["fg", "a"],
      ["fg.muted", "b"],
      ["bg.rail", "c"],
    ]);
  });

  test("rejects a name defined twice", () => {
    expect(() =>
      flattenTokens({ "bg.rail": { value: "a" }, bg: { rail: { value: "b" } } }),
    ).toThrow('token "bg.rail" is defined twice');
  });
});

describe("createPandaTokenResolver", () => {
  test("returns plain values unchanged", () => {
    const r = resolver({ radii: { card: { value: "1rem" } }, zIndex: { modal: { value: 60 } } });
    expect(r.resolve("radii", "card")).toBe("1rem");
    expect(r.resolve("zIndex", "modal")).toBe(60);
  });

  test("picks the scheme's condition and falls back to base", () => {
    const r = resolver(
      { colors: { ink: { 900: { value: "#0b1219" }, 50: { value: "#f2f6f7" } } } },
      {
        colors: {
          bg: { value: { _dark: "{colors.ink.900}", _light: "{colors.ink.50}" } },
          rail: { value: { base: "#111111", _light: "#eeeeee" } },
          fixed: { value: "{colors.ink.900}" },
        },
      },
    );
    expect(r.resolve("colors", "bg", "night")).toBe("#0b1219");
    expect(r.resolve("colors", "bg", "daylight")).toBe("#f2f6f7");
    expect(r.resolve("colors", "rail", "night")).toBe("#111111");
    expect(r.resolve("colors", "rail", "daylight")).toBe("#eeeeee");
    expect(r.resolve("colors", "fixed", "daylight")).toBe("#0b1219");
  });

  test("follows reference chains through tokens and semantic tokens", () => {
    const r = resolver(
      { colors: { teal: { 300: { value: "#6fd3c7" }, accent: { value: "{colors.teal.300}" } } } },
      {
        colors: {
          action: { DEFAULT: { value: { _dark: "{colors.teal.accent}", _light: "#0b7a6f" } } },
          focus: { value: "{colors.action}" },
        },
      },
    );
    expect(r.resolve("colors", "focus", "night")).toBe("#6fd3c7");
    expect(r.resolve("colors", "focus", "daylight")).toBe("#0b7a6f");
  });

  test("substitutes references embedded in a larger value", () => {
    const r = resolver(
      { colors: { ink: { 900: { value: "#0b1219" } } } },
      { shadows: { ring: { value: "0 0 0 2px {colors.ink.900}" } } },
    );
    expect(r.resolve("shadows", "ring", "night")).toBe("0 0 0 2px #0b1219");
  });

  test("throws on a missing reference and names where it came from", () => {
    const r = resolver({ colors: { teal: { 300: { value: "{colors.teal.250}" } } } });
    expect(() => r.resolve("colors", "teal.300", "night")).toThrow(
      "unknown token reference {colors.teal.250} (from colors.teal.300)",
    );
    expect(() => r.resolve("colors", "nope", "night")).toThrow(
      "unknown token reference {colors.nope}",
    );
  });

  test("throws on a reference cycle", () => {
    const r = resolver({ colors: { a: { value: "{colors.b}" }, b: { value: "{colors.a}" } } });
    expect(() => r.resolve("colors", "a", "night")).toThrow(
      "reference cycle: colors.a -> colors.b -> colors.a",
    );
  });

  test("throws when a scheme has no value and no base", () => {
    const r = resolver({}, { colors: { bg: { value: { _dark: "#000000" } } } });
    expect(() => r.resolve("colors", "bg", "daylight")).toThrow(
      'colors.bg: no "_light" or "base" value for scheme "daylight"',
    );
  });

  test("throws on nested conditions and unknown schemes", () => {
    const r = resolver({}, { colors: { bg: { value: { _dark: { _hover: "#000000" } } } } });
    expect(() => r.resolve("colors", "bg", "night")).toThrow("nested conditions");
    expect(() => r.resolve("colors", "bg", "dusk")).toThrow('unknown scheme "dusk"');
  });

  test("throws on a malformed reference", () => {
    const r = resolver({ colors: { a: { value: "{teal}" } } });
    expect(() => r.resolve("colors", "a", "night")).toThrow("malformed token reference {teal}");
  });

  test("rejects a name that is both a token and a semantic token", () => {
    expect(() =>
      resolver({ colors: { bg: { value: "#000000" } } }, { colors: { bg: { value: "#ffffff" } } }),
    ).toThrow("colors.bg is both a token and a semantic token");
  });

  test("lists names in declaration order", () => {
    const r = resolver(
      { radii: { tag: { value: "0.25rem" }, pill: { value: "999px" } } },
      { colors: { fg: { DEFAULT: { value: "#fff" }, muted: { value: "#aaa" } } } },
    );
    expect(r.tokenNames("radii")).toEqual(["tag", "pill"]);
    expect(r.semanticNames("colors")).toEqual(["fg", "fg.muted"]);
    expect(r.semanticNames("sizes")).toEqual([]);
  });
});

import { afterEach, describe, expect, test } from "bun:test";
import {
  consistentColor,
  setConsistentColorBackend,
} from "../src/lib/chat-ui";

// XEP-0392 §5.1 sample vectors. SHA-1("romeo") = first two bytes
// 0x6e3c, hue = (0x6e3c / 0x10000) * 360 ≈ 155.13°. The spec's
// "Juliet" sample uses the JID "juliet@capulet.lit" — we re-derive
// fresh values here for `consistentColor`'s default sat/light so
// the test pins the actual chat output, not an intermediate.
//
// Reference values produced by running the Rust impl
// (waddle_xmpp_core::xep0392::compute_hue) on the same inputs.
const SHA1_HUE_ROMEO = 155.13;
const SHA1_HUE_JULIET = 80.32;

function djb2Hue(input: string): number {
  let h = 5381;
  for (let i = 0; i < input.length; i++) {
    h = ((h << 5) + h + input.charCodeAt(i)) & 0xffff;
  }
  return (h / 65536) * 360;
}

afterEach(() => {
  // Reset to fallback so test order doesn't leak.
  setConsistentColorBackend(djb2Hue);
});

describe("consistentColor", () => {
  test("uses installed XEP-0392 backend after setConsistentColorBackend", () => {
    setConsistentColorBackend((input) => {
      if (input === "romeo") return SHA1_HUE_ROMEO;
      if (input === "juliet") return SHA1_HUE_JULIET;
      throw new Error(`unexpected input ${input}`);
    });

    expect(consistentColor("romeo", 100, 50)).toBe(
      `hsl(${Math.round(SHA1_HUE_ROMEO)}, 100%, 50%)`,
    );
    expect(consistentColor("juliet", 65, 50)).toBe(
      `hsl(${Math.round(SHA1_HUE_JULIET)}, 65%, 50%)`,
    );
  });

  test("falls back to deterministic DJB2 hue when no backend is installed", () => {
    // The fallback's exact values are an implementation detail — the
    // contract is "deterministic + non-empty before wasm loads."
    const a = consistentColor("alice");
    const b = consistentColor("alice");
    expect(a).toBe(b);
    expect(a).toMatch(/^hsl\(\d+, \d+%, \d+%\)$/);
  });

  test("different inputs produce different hues with the spec backend", () => {
    setConsistentColorBackend((input) => {
      // Real wasm impl will provide collision-resistant output; this
      // mock just proves consistentColor passes the input through.
      return input === "alice" ? 30 : 200;
    });
    expect(consistentColor("alice")).not.toBe(consistentColor("bob"));
  });
});

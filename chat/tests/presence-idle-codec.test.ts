import { describe, expect, test } from "bun:test";

import { mapPresenceIdleSince } from "../src/lib/xmpp/wasm-message-codecs";
import type { WasmPresence } from "../src/lib/xmpp/wasm-types";

function presence(idle_since?: string): WasmPresence {
  return { presence_type: "available", hats: [], ...(idle_since ? { idle_since } : {}) };
}

describe("mapPresenceIdleSince", () => {
  test("parses an XEP-0319 since into epoch ms", () => {
    expect(mapPresenceIdleSince(presence("1969-07-21T02:56:15Z"))).toBe(
      Date.parse("1969-07-21T02:56:15Z"),
    );
  });

  test("a presence with no idle stamp yields undefined", () => {
    expect(mapPresenceIdleSince(presence())).toBeUndefined();
  });

  test("a malformed since degrades to undefined, never NaN", () => {
    const result = mapPresenceIdleSince(presence("not-a-date"));
    expect(result).toBeUndefined();
  });
});

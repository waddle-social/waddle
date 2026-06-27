import { describe, expect, test } from "bun:test";

import {
  mapPresenceIdleSince,
  mapPresenceShow,
  parsePresenceShow,
} from "../src/lib/xmpp/wasm-message-codecs";
import type { WasmPresence } from "../src/lib/xmpp/wasm-types";

function presence(idle_since?: string): WasmPresence {
  return { presence_type: "available", hats: [], ...(idle_since ? { idle_since } : {}) };
}

function presenceWithShow(show: string): WasmPresence {
  return { presence_type: "available", hats: [], show };
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

// ADR-010 fold: an incoming `<show>chat</show>` ("free for chat") is a
// substate of Available, so the receiver renders it as Available rather than
// minting a new rendered Show. These guard that the fold is deliberate — a
// later refactor of the codec must not silently surface or drop `chat`.
describe("inbound chat folds into available", () => {
  test("mapPresenceShow folds a contact's chat into available", () => {
    expect(mapPresenceShow(presenceWithShow("chat"))).toBe("available");
  });

  test("parsePresenceShow folds a MUC occupant's chat into online", () => {
    expect(parsePresenceShow("chat")).toBe("online");
  });
});

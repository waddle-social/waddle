import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "@/lib/chat-ui";
import { resolveRoomMessageTarget } from "@/lib/messaging/room-message-target";

const first: TimelineMessage = {
  id: "room-id", stanzaId: "room-id", stanzaIdBy: "room@muc.example.com",
  author: "alice", authorOccupantJid: "room@muc.example.com/alice",
  body: "first", createdAt: "2026-07-01T10:00:00Z", isSelf: false,
};

describe("room reference resolution", () => {
  test("protocol references prefer verified canonical room identity", () => {
    const claimant = { ...first, stanzaId: undefined, stanzaIdBy: undefined };
    expect(resolveRoomMessageTarget([claimant, first], "room-id").message).toBe(first);
    expect(resolveRoomMessageTarget([claimant, first], "room-id", { preferCanonical: false }).ambiguous).toBe(true);
  });

  test("foreign stanza authority cannot disambiguate duplicate raw claims", () => {
    const foreign = { ...first, stanzaIdBy: "foreign.example.com" };
    const claimant = { ...first, stanzaId: undefined, stanzaIdBy: undefined };
    expect(resolveRoomMessageTarget([foreign, claimant], "room-id").ambiguous).toBe(true);
  });

  test("unverified raw IDs and aliases must identify exactly one row", () => {
    const alias = { ...first, id: "different-id", stanzaId: undefined, stanzaIdBy: undefined, wireIds: ["room-id"] };
    expect(resolveRoomMessageTarget([alias], "room-id").message).toBe(alias);
    expect(resolveRoomMessageTarget([alias, { ...alias }], "room-id").ambiguous).toBe(true);
    expect(resolveRoomMessageTarget([], "room-id")).toEqual({ ambiguous: false });
  });
});

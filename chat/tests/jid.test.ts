import { describe, expect, test } from "bun:test";
import { jidDomain, parseManagedRoomBareJid, roomBareJidFor } from "../src/lib/xmpp/jid";

describe("jid helpers", () => {
  test("roomBareJidFor uses the bare account domain", () => {
    expect(roomBareJidFor(
      {
        username: "alice",
        jid: "alice@example.com/desktop",
        session_id: "tok",
        xmpp_websocket_url: "wss://example.com/xmpp",
      },
      "w1",
      "roadmap",
    )).toBe("w1_roadmap@muc.example.com");
    expect(jidDomain("alice@example.com/desktop")).toBe("example.com");
  });

  test("parseManagedRoomBareJid extracts the canonical managed room identifiers", () => {
    expect(parseManagedRoomBareJid("w1_road_map@muc.example.com/Alice")).toEqual({
      waddleId: "w1",
      channelId: "road_map",
    });
    expect(parseManagedRoomBareJid("roadmap@muc.example.com")).toBeNull();
  });
});

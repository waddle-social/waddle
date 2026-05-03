import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { roomJidForChannelId, roomJidForChannelSummary } from "../src/lib/channel-room";
import type { WaddleSession } from "../src/lib/server-auth";

const session = {
  jid: "alice@example.com/desktop",
  username: "alice",
  token: "token",
} as WaddleSession;

describe("roomJidForChannelSummary", () => {
  test("uses the discovered channel JID when present", () => {
    expect(roomJidForChannelSummary(session, {
      id: "general",
      jid: "general@conference.example.net",
    })).toBe("general@conference.example.net");
  });

  test("strips any accidental resource from the discovered channel JID", () => {
    expect(roomJidForChannelSummary(session, {
      id: "general",
      jid: "general@conference.example.net/alice",
    })).toBe("general@conference.example.net");
  });

  test("falls back to the managed room JID when discovery has no JID", () => {
    expect(roomJidForChannelSummary(session, { id: "general" })).toBe("general@muc.example.com");
  });

  test("resolves active channel ids through discovered topology", () => {
    expect(roomJidForChannelId(session, [
      { id: "general", jid: "general@conference.example.net" },
      { id: "random", jid: "random@conference.example.net" },
    ], "random")).toBe("random@conference.example.net");
  });

  test("ChatApp routes unread and activity clearing through the discovered room resolver", () => {
    const source = readFileSync(
      new URL("../src/components/ChatApp.vue", import.meta.url),
      "utf8",
    );

    expect(source).toContain("roomJidForChannelId as resolveRoomJidForChannelId");
    expect(source).toContain("readReceiptsActiveRoomJid");
    expect(source).toContain("resolveRoomJidForChannelId(sess, waddles.channels.value, channelId)");
    expect(source).toContain("channelUnread.markThreadRead(roomJid, threadId)");
    expect(source).toContain("messaging.clearChannelActivity(roomJid)");
    expect(source).not.toContain("roomBareJidFor");
  });
});

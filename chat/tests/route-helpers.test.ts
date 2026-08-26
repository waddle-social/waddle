import { describe, expect, test } from "bun:test";
import { resolveChannelBySlug, resolveRoomByDmUsername } from "../src/shell/route-helpers";
import type { ChannelSummary } from "../src/lib/chat-types";

const baseChannel = { id: "room-id-1", name: "general" } as ChannelSummary;

describe("resolveChannelBySlug", () => {
  test("matches by id, not by display name", () => {
    const channels = [
      { ...baseChannel, id: "space-a-general", name: "general" },
      { ...baseChannel, id: "space-b-general", name: "general" },
    ];
    expect(resolveChannelBySlug("space-b-general", channels)?.id).toBe("space-b-general");
    expect(resolveChannelBySlug("general", channels)).toBeUndefined();
  });
});

describe("resolveRoomByDmUsername", () => {
  test("treats a DM username as a room when the slug matches a channel id", () => {
    const channels = [
      { ...baseChannel, id: "chat", name: "Chat" },
      { ...baseChannel, id: "forum", name: "Forum" },
    ];
    expect(resolveRoomByDmUsername("chat", channels)?.id).toBe("chat");
    expect(resolveRoomByDmUsername("@chat", channels)?.id).toBe("chat");
    expect(resolveRoomByDmUsername("Chat", channels)?.id).toBe("chat");
    expect(resolveRoomByDmUsername("bob", channels)).toBeUndefined();
  });
});

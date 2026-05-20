import { describe, expect, test } from "bun:test";
import { resolveChannelBySlug } from "../src/shell/route-helpers";
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

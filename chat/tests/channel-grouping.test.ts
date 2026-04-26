import { describe, expect, test } from "bun:test";
import { groupChannelsBySpace } from "../src/lib/channel-grouping";

describe("groupChannelsBySpace", () => {
  test("groups rooms under spaces and keeps standalone rooms separate", () => {
    const groups = groupChannelsBySpace(
      [{ id: "general", name: "General" }],
      [
        { id: "chat", name: "Chat", spaceId: "general", position: 0 },
        { id: "random", name: "Random", position: 1 },
      ],
    );

    expect(groups).toEqual([
      {
        id: "general",
        name: "General",
        space: { id: "general", name: "General" },
        channels: [{ id: "chat", name: "Chat", spaceId: "general", position: 0 }],
      },
      {
        id: "standalone",
        name: "Channels",
        space: null,
        channels: [{ id: "random", name: "Random", position: 1 }],
      },
    ]);
  });
});

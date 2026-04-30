import { describe, expect, test } from "bun:test";
import {
  firstChannelIdForSpace,
  firstStandaloneChannelId,
  groupChannelsBySpace,
  STANDALONE_CHANNEL_GROUP_ID,
  STANDALONE_CHANNEL_GROUP_NAME,
} from "../src/lib/channel-grouping";
import type { ChannelSummary } from "../src/lib/chat-types";

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
        id: "space:general",
        name: "General",
        space: { id: "general", name: "General" },
        channels: [{ id: "chat", name: "Chat", spaceId: "general", position: 0 }],
      },
      {
        id: STANDALONE_CHANNEL_GROUP_ID,
        name: STANDALONE_CHANNEL_GROUP_NAME,
        space: null,
        channels: [{ id: "random", name: "Random", position: 1 }],
      },
    ]);
  });

  test("keeps empty spaces visible as collapsible groups", () => {
    const groups = groupChannelsBySpace(
      [
        { id: "empty", name: "Empty" },
        { id: "general", name: "General" },
      ],
      [{ id: "chat", name: "Chat", spaceId: "general", position: 0 }],
    );

    expect(groups).toEqual([
      {
        id: "space:empty",
        name: "Empty",
        space: { id: "empty", name: "Empty" },
        channels: [],
      },
      {
        id: "space:general",
        name: "General",
        space: { id: "general", name: "General" },
        channels: [{ id: "chat", name: "Chat", spaceId: "general", position: 0 }],
      },
    ]);
  });

  test("labels standalone groups as standalone channels", () => {
    const groups = groupChannelsBySpace(
      [],
      [{ id: "random", name: "Random", position: 0 }],
    );

    expect(groups).toHaveLength(1);
    expect(groups[0]?.id).toBe(STANDALONE_CHANNEL_GROUP_ID);
    expect(groups[0]?.name).toBe("Standalone channels");
    expect(groups[0]?.space).toBeNull();
  });
});

describe("channel container routing helpers", () => {
  const channels: ChannelSummary[] = [
    { id: "standalone-late", name: "Standalone Late", position: 20 },
    { id: "space-late", name: "Space Late", spaceId: "space-a", position: 20 },
    { id: "standalone-first", name: "Standalone First", position: 1 },
    { id: "space-first", name: "Space First", spaceId: "space-a", position: 1 },
    { id: "other-space", name: "Other Space", spaceId: "space-b", position: 0 },
  ];

  test("choosing a Space maps to the first channel in that Space", () => {
    expect(firstChannelIdForSpace(channels, "space-a")).toBe("space-first");
  });

  test("choosing the standalone container maps to the first standalone channel", () => {
    expect(firstStandaloneChannelId(channels)).toBe("standalone-first");
  });

  test("container helpers return null when there is no matching channel", () => {
    expect(firstChannelIdForSpace(channels, "missing-space")).toBeNull();
    expect(firstStandaloneChannelId([{ id: "space-only", name: "Space Only", spaceId: "space-a" }])).toBeNull();
  });
});

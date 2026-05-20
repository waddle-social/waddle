import { describe, expect, test } from "bun:test";
import { matchLocation } from "../src/router";
import {
  resolveChannelBySlug,
  shouldLoadStructureForMatch,
} from "../src/shell/route-helpers";
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

describe("shouldLoadStructureForMatch", () => {
  test("loads topology on a cold direct channel refresh", () => {
    expect(shouldLoadStructureForMatch(matchLocation("/r/room-id-1"), 0)).toBe(true);
  });

  test("loads topology on a cold extension route refresh", () => {
    expect(
      shouldLoadStructureForMatch(matchLocation("/r/room-id-1/x/link-board/saved-links"), 0),
    ).toBe(true);
  });

  test("loads topology on the home route when nothing is hydrated", () => {
    expect(shouldLoadStructureForMatch(matchLocation("/"), 0)).toBe(true);
  });

  test("skips topology when channel data is already present", () => {
    expect(shouldLoadStructureForMatch(matchLocation("/r/room-id-1"), 2)).toBe(false);
  });

  test("does not load channel topology for DM routes", () => {
    expect(shouldLoadStructureForMatch(matchLocation("/dm/alice"), 0)).toBe(false);
  });

  test("does not load channel topology for settings / threads / admin", () => {
    expect(shouldLoadStructureForMatch(matchLocation("/settings"), 0)).toBe(false);
    expect(shouldLoadStructureForMatch(matchLocation("/threads"), 0)).toBe(false);
    expect(shouldLoadStructureForMatch(matchLocation("/admin/users"), 0)).toBe(false);
  });
});

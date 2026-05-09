import { describe, expect, test } from "bun:test";
import { buildDirectMessagePath, buildChannelExtensionPath, buildChannelPath, buildChatSettingsPath, parseChatLocation, resolveChannelBySlug, shouldLoadWaddleStructureForRoute } from "../src/shell/navigation";
import type { ChannelSummary } from "../src/lib/chat-types";

const channel: ChannelSummary = {
  id: "room-id-1",
  name: "general",
} as ChannelSummary;

describe("parseChatLocation / buildChannelPath threadStack", () => {
  test("parses the dedicated settings route", () => {
    const route = parseChatLocation("/settings");
    expect(route.page).toBe("settings");
    expect(route.channelSlug).toBeNull();
    expect(route.dmUsername).toBeNull();
    expect(route.threadStack).toEqual([]);
  });

  test("buildChatSettingsPath returns the settings page path", () => {
    expect(buildChatSettingsPath()).toBe("/settings");
  });

  test("reads an empty thread stack when the query param is missing", () => {
    const route = parseChatLocation("/r/room-id-1", "");
    expect(route.page).toBe("chat");
    expect(route.channelSlug).toBe("room-id-1");
    expect(route.threadStack).toEqual([]);
  });

  test("parses comma-separated thread ids with URL decoding", () => {
    const route = parseChatLocation("/r/room-id-1", "?thread=root-abc,child-def");
    expect(route.threadStack).toEqual(["root-abc", "child-def"]);
  });

  test("ignores empty segments in the thread query", () => {
    const route = parseChatLocation("/r/room-id-1", "?thread=root,,,child");
    expect(route.threadStack).toEqual(["root", "child"]);
  });

  test("buildChannelPath appends ?thread=... when a stack is supplied", () => {
    const path = buildChannelPath(channel, ["root-abc", "child-def"]);
    expect(path).toBe("/r/room-id-1?thread=root-abc,child-def");
  });

  test("buildChannelPath drops the query when the stack is empty", () => {
    const path = buildChannelPath(channel, []);
    expect(path).toBe("/r/room-id-1");
  });

  test("buildChannelPath round-trips through parseChatLocation", () => {
    const stack = ["A", "B", "C"];
    const path = buildChannelPath(channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseChatLocation(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
    expect(route.channelSlug).toBe("room-id-1");
  });

  test("buildChannelPath uses the globally unique channel id instead of the display name", () => {
    expect(buildChannelPath({ ...channel, id: "space-a-general", name: "general" })).toBe("/r/space-a-general");
    expect(buildChannelPath({ ...channel, id: "space-b-general", name: "general" })).toBe("/r/space-b-general");
  });

  test("buildChannelExtensionPath targets a route under the channel", () => {
    const path = buildChannelExtensionPath(channel, "link-board", "saved-links");
    expect(path).toBe("/r/room-id-1/x/link-board/saved-links");
    const route = parseChatLocation(path);
    expect(route).toMatchObject({
      page: "extension",
      channelSlug: "room-id-1",
      extensionPluginId: "link-board",
      extensionRouteId: "saved-links",
      threadStack: [],
    });
  });

  test("buildChannelExtensionPath preserves collapsed right-panel state", () => {
    const path = buildChannelExtensionPath(channel, "link-board", "saved-links", ["root,1", "child%2"], true);
    expect(path).toBe("/r/room-id-1/x/link-board/saved-links?thread=root%2C1,child%252&pinned=1");
    const route = parseChatLocation("/r/room-id-1/x/link-board/saved-links", "?thread=root%2C1,child%252&pinned=1");
    expect(route).toMatchObject({
      page: "extension",
      threadStack: ["root,1", "child%2"],
      pinnedPanelOpen: true,
    });
  });

  test("resolveChannelBySlug uses channel ids only", () => {
    const channels = [
      { ...channel, id: "space-a-general", name: "general" },
      { ...channel, id: "space-b-general", name: "general" },
    ];
    expect(resolveChannelBySlug("space-b-general", channels)?.id).toBe("space-b-general");
    expect(resolveChannelBySlug("general", channels)).toBeUndefined();
  });

  test("DM paths preserve XMPP localpart characters", () => {
    const path = buildDirectMessagePath("first.last_user+test");
    expect(path).toBe("/dm/first.last_user%2Btest");
    const route = parseChatLocation(path);
    expect(route.dmUsername).toBe("first.last_user+test");
  });

  test("round-trips ids that contain URL-reserved characters", () => {
    const stack = ["id with spaces", "id/with/slashes"];
    const path = buildChannelPath(channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseChatLocation(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });

  test("round-trips ids containing a literal comma", () => {
    // Commas are the stack separator, so buildChannelPath must percent-encode them
    // (%2C) and parseChatLocation must decode back to the original comma without
    // splitting on it.
    const stack = ["before,after", "plain"];
    const path = buildChannelPath(channel, stack);
    expect(path).toContain("%2C");
    const [pathname, search] = path.split("?");
    const route = parseChatLocation(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });

  test("round-trips ids containing a literal percent sign", () => {
    // `%` must be encoded to `%25` and decoded exactly once — a naive
    // double-decode would turn `%2520` back into a space instead of `%20`.
    const stack = ["100%", "a%20b"];
    const path = buildChannelPath(channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseChatLocation(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });
});

describe("shouldLoadWaddleStructureForRoute", () => {
  test("loads topology on a cold direct channel refresh", () => {
    expect(shouldLoadWaddleStructureForRoute(parseChatLocation("/r/room-id-1"), 0)).toBe(true);
  });

  test("loads topology on a cold direct extension route refresh", () => {
    expect(shouldLoadWaddleStructureForRoute(parseChatLocation("/r/room-id-1/x/link-board/saved-links"), 0)).toBe(true);
  });

  test("loads topology on the dashboard route when nothing is hydrated", () => {
    expect(shouldLoadWaddleStructureForRoute(parseChatLocation("/"), 0)).toBe(true);
  });

  test("skips topology when channel data is already present", () => {
    expect(shouldLoadWaddleStructureForRoute(parseChatLocation("/r/room-id-1"), 2)).toBe(false);
  });

  test("does not load channel topology for DM routes", () => {
    expect(shouldLoadWaddleStructureForRoute(parseChatLocation("/dm/alice"), 0)).toBe(false);
  });
});

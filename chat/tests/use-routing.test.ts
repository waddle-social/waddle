import { describe, expect, test } from "bun:test";
import { buildDmPath, buildExtensionRoutePath, buildPath, buildSettingsPath, parseRoute, resolveChannel, shouldLoadStructureForRoute } from "../src/composables/useRouting";
import type { ChannelSummary } from "../src/lib/chat-types";

const channel: ChannelSummary = {
  id: "room-id-1",
  name: "general",
} as ChannelSummary;

describe("parseRoute / buildPath threadStack", () => {
  test("parses the dedicated settings route", () => {
    const route = parseRoute("/settings");
    expect(route.page).toBe("settings");
    expect(route.channelSlug).toBeNull();
    expect(route.dmUsername).toBeNull();
    expect(route.threadStack).toEqual([]);
  });

  test("buildSettingsPath returns the settings page path", () => {
    expect(buildSettingsPath()).toBe("/settings");
  });

  test("reads an empty thread stack when the query param is missing", () => {
    const route = parseRoute("/r/room-id-1", "");
    expect(route.page).toBe("chat");
    expect(route.channelSlug).toBe("room-id-1");
    expect(route.threadStack).toEqual([]);
  });

  test("parses comma-separated thread ids with URL decoding", () => {
    const route = parseRoute("/r/room-id-1", "?thread=root-abc,child-def");
    expect(route.threadStack).toEqual(["root-abc", "child-def"]);
  });

  test("ignores empty segments in the thread query", () => {
    const route = parseRoute("/r/room-id-1", "?thread=root,,,child");
    expect(route.threadStack).toEqual(["root", "child"]);
  });

  test("buildPath appends ?thread=... when a stack is supplied", () => {
    const path = buildPath(channel, ["root-abc", "child-def"]);
    expect(path).toBe("/r/room-id-1?thread=root-abc,child-def");
  });

  test("buildPath drops the query when the stack is empty", () => {
    const path = buildPath(channel, []);
    expect(path).toBe("/r/room-id-1");
  });

  test("buildPath round-trips through parseRoute", () => {
    const stack = ["A", "B", "C"];
    const path = buildPath(channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseRoute(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
    expect(route.channelSlug).toBe("room-id-1");
  });

  test("buildPath uses the globally unique channel id instead of the display name", () => {
    expect(buildPath({ ...channel, id: "space-a-general", name: "general" })).toBe("/r/space-a-general");
    expect(buildPath({ ...channel, id: "space-b-general", name: "general" })).toBe("/r/space-b-general");
  });

  test("buildExtensionRoutePath targets a route under the channel", () => {
    const path = buildExtensionRoutePath(channel, "link-board", "saved-links");
    expect(path).toBe("/r/room-id-1/x/link-board/saved-links");
    const route = parseRoute(path);
    expect(route).toMatchObject({
      page: "extension",
      channelSlug: "room-id-1",
      extensionPluginId: "link-board",
      extensionRouteId: "saved-links",
      threadStack: [],
    });
  });

  test("resolveChannel uses channel ids only", () => {
    const channels = [
      { ...channel, id: "space-a-general", name: "general" },
      { ...channel, id: "space-b-general", name: "general" },
    ];
    expect(resolveChannel("space-b-general", channels)?.id).toBe("space-b-general");
    expect(resolveChannel("general", channels)).toBeUndefined();
  });

  test("DM paths preserve XMPP localpart characters", () => {
    const path = buildDmPath("first.last_user+test");
    expect(path).toBe("/dm/first.last_user%2Btest");
    const route = parseRoute(path);
    expect(route.dmUsername).toBe("first.last_user+test");
  });

  test("round-trips ids that contain URL-reserved characters", () => {
    const stack = ["id with spaces", "id/with/slashes"];
    const path = buildPath(channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseRoute(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });

  test("round-trips ids containing a literal comma", () => {
    // Commas are the stack separator, so buildPath must percent-encode them
    // (%2C) and parseRoute must decode back to the original comma without
    // splitting on it.
    const stack = ["before,after", "plain"];
    const path = buildPath(channel, stack);
    expect(path).toContain("%2C");
    const [pathname, search] = path.split("?");
    const route = parseRoute(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });

  test("round-trips ids containing a literal percent sign", () => {
    // `%` must be encoded to `%25` and decoded exactly once — a naive
    // double-decode would turn `%2520` back into a space instead of `%20`.
    const stack = ["100%", "a%20b"];
    const path = buildPath(channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseRoute(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });
});

describe("shouldLoadStructureForRoute", () => {
  test("loads topology on a cold direct channel refresh", () => {
    expect(shouldLoadStructureForRoute(parseRoute("/r/room-id-1"), 0)).toBe(true);
  });

  test("loads topology on a cold direct extension route refresh", () => {
    expect(shouldLoadStructureForRoute(parseRoute("/r/room-id-1/x/link-board/saved-links"), 0)).toBe(true);
  });

  test("loads topology on the dashboard route when nothing is hydrated", () => {
    expect(shouldLoadStructureForRoute(parseRoute("/"), 0)).toBe(true);
  });

  test("skips topology when channel data is already present", () => {
    expect(shouldLoadStructureForRoute(parseRoute("/r/room-id-1"), 2)).toBe(false);
  });

  test("does not load channel topology for DM routes", () => {
    expect(shouldLoadStructureForRoute(parseRoute("/dm/alice"), 0)).toBe(false);
  });
});

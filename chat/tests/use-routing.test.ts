import { describe, expect, test } from "bun:test";
import { buildPath, buildSettingsPath, parseRoute } from "../src/composables/useRouting";
import type { ChannelSummary, WaddleSummary } from "../src/lib/waddle-api";

const waddle: WaddleSummary = {
  id: "w1",
  name: "My Waddle",
} as WaddleSummary;

const channel: ChannelSummary = {
  id: "c1",
  name: "general",
} as ChannelSummary;

describe("parseRoute / buildPath threadStack", () => {
  test("parses the dedicated settings route", () => {
    const route = parseRoute("/settings");
    expect(route.page).toBe("settings");
    expect(route.waddleSlug).toBeNull();
    expect(route.channelSlug).toBeNull();
    expect(route.dmUsername).toBeNull();
    expect(route.threadStack).toEqual([]);
  });

  test("buildSettingsPath returns the settings page path", () => {
    expect(buildSettingsPath()).toBe("/settings");
  });

  test("reads an empty thread stack when the query param is missing", () => {
    const route = parseRoute("/my-waddle/general", "");
    expect(route.page).toBe("chat");
    expect(route.waddleSlug).toBe("my-waddle");
    expect(route.channelSlug).toBe("general");
    expect(route.threadStack).toEqual([]);
  });

  test("parses comma-separated thread ids with URL decoding", () => {
    const route = parseRoute("/my-waddle/general", "?thread=root-abc,child-def");
    expect(route.threadStack).toEqual(["root-abc", "child-def"]);
  });

  test("ignores empty segments in the thread query", () => {
    const route = parseRoute("/my-waddle/general", "?thread=root,,,child");
    expect(route.threadStack).toEqual(["root", "child"]);
  });

  test("buildPath appends ?thread=... when a stack is supplied", () => {
    const path = buildPath(waddle, channel, ["root-abc", "child-def"]);
    expect(path).toBe("/my-waddle/general?thread=root-abc,child-def");
  });

  test("buildPath drops the query when the stack is empty", () => {
    const path = buildPath(waddle, channel, []);
    expect(path).toBe("/my-waddle/general");
  });

  test("buildPath round-trips through parseRoute", () => {
    const stack = ["A", "B", "C"];
    const path = buildPath(waddle, channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseRoute(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
    expect(route.waddleSlug).toBe("my-waddle");
    expect(route.channelSlug).toBe("general");
  });

  test("round-trips ids that contain URL-reserved characters", () => {
    const stack = ["id with spaces", "id/with/slashes"];
    const path = buildPath(waddle, channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseRoute(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });

  test("round-trips ids containing a literal comma", () => {
    // Commas are the stack separator, so buildPath must percent-encode them
    // (%2C) and parseRoute must decode back to the original comma without
    // splitting on it.
    const stack = ["before,after", "plain"];
    const path = buildPath(waddle, channel, stack);
    expect(path).toContain("%2C");
    const [pathname, search] = path.split("?");
    const route = parseRoute(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });

  test("round-trips ids containing a literal percent sign", () => {
    // `%` must be encoded to `%25` and decoded exactly once — a naive
    // double-decode would turn `%2520` back into a space instead of `%20`.
    const stack = ["100%", "a%20b"];
    const path = buildPath(waddle, channel, stack);
    const [pathname, search] = path.split("?");
    const route = parseRoute(pathname, search ? `?${search}` : "");
    expect(route.threadStack).toEqual(stack);
  });
});

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { buildHref, matchLocation, navigate, routes, type RouteMatch } from "../src/router";

describe("matchLocation", () => {
  test("returns home for '/'", () => {
    const m = matchLocation("/", "");
    expect(m).toEqual({ id: "home" });
  });

  test("falls back to home for unknown paths", () => {
    expect(matchLocation("/some/unknown/path", "")).toEqual({ id: "home" });
    expect(matchLocation("", "")).toEqual({ id: "home" });
  });

  test("parses settings", () => {
    expect(matchLocation("/settings", "")).toEqual({ id: "settings" });
  });

  test("parses threads", () => {
    expect(matchLocation("/threads", "")).toEqual({ id: "threads" });
  });

  test("parses channel with no search", () => {
    const m = matchLocation("/r/room-id-1", "");
    expect(m).toEqual({
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: [], pinned: false },
    });
  });

  test("parses channel with thread stack and pinned flag", () => {
    const m = matchLocation("/r/room-id-1", "?thread=root-abc,child-def&pinned=1");
    expect(m).toEqual({
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: ["root-abc", "child-def"], pinned: true },
    });
  });

  test("parses channelExtension and beats the bare channel route", () => {
    const m = matchLocation("/r/room-id-1/x/link-board/saved-links", "?thread=root&pinned=1");
    expect(m).toEqual({
      id: "channelExtension",
      params: { channelId: "room-id-1", pluginId: "link-board", routeId: "saved-links" },
      search: { thread: ["root"], pinned: true },
    });
  });

  test("parses dm and preserves XMPP localpart characters", () => {
    const m = matchLocation("/dm/first.last_user%2Btest", "");
    expect(m).toEqual({
      id: "dm",
      params: { username: "first.last_user+test" },
      search: { thread: [] },
    });
  });

  test("parses admin with explicit panel", () => {
    expect(matchLocation("/admin/spaces", "")).toEqual({
      id: "admin",
      params: { panel: "spaces" },
    });
  });

  test("parses admin without a panel as users", () => {
    expect(matchLocation("/admin", "")).toEqual({
      id: "admin",
      params: { panel: "users" },
    });
  });

  test("unknown admin panel falls back to users", () => {
    expect(matchLocation("/admin/foobar", "")).toEqual({
      id: "admin",
      params: { panel: "users" },
    });
  });

  test("ignores empty segments in the thread stack", () => {
    const m = matchLocation("/r/room-id-1", "?thread=root,,,child");
    expect(m).toEqual({
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: ["root", "child"], pinned: false },
    });
  });
});

describe("buildHref", () => {
  test("home", () => {
    expect(buildHref({ id: "home" })).toBe("/");
  });

  test("threads", () => {
    expect(buildHref({ id: "threads" })).toBe("/threads");
  });

  test("settings (with and without origin)", () => {
    expect(buildHref({ id: "settings" })).toBe("/settings");
    expect(buildHref({ id: "settings", origin: "app" })).toBe("/settings");
  });

  test("channel without search drops the query", () => {
    const href = buildHref({
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: [], pinned: false },
    });
    expect(href).toBe("/r/room-id-1");
  });

  test("channel with thread stack", () => {
    const href = buildHref({
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: ["root-abc", "child-def"], pinned: false },
    });
    expect(href).toBe("/r/room-id-1?thread=root-abc,child-def");
  });

  test("channel with pinned flag only", () => {
    const href = buildHref({
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: [], pinned: true },
    });
    expect(href).toBe("/r/room-id-1?pinned=1");
  });

  test("channel with thread stack and pinned flag", () => {
    const href = buildHref({
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: ["root-abc"], pinned: true },
    });
    expect(href).toBe("/r/room-id-1?thread=root-abc&pinned=1");
  });

  test("channelExtension preserves all search params", () => {
    const href = buildHref({
      id: "channelExtension",
      params: { channelId: "room-id-1", pluginId: "link-board", routeId: "saved-links" },
      search: { thread: ["root,1", "child%2"], pinned: true },
    });
    expect(href).toBe("/r/room-id-1/x/link-board/saved-links?thread=root%2C1,child%252&pinned=1");
  });

  test("dm encodes XMPP localpart specials", () => {
    const href = buildHref({
      id: "dm",
      params: { username: "first.last_user+test" },
      search: { thread: [] },
    });
    expect(href).toBe("/dm/first.last_user%2Btest");
  });

  test("admin uses the panel slug", () => {
    expect(buildHref({ id: "admin", params: { panel: "spaces" } })).toBe("/admin/spaces");
    expect(buildHref({ id: "admin", params: { panel: "users" } })).toBe("/admin/users");
  });
});

describe("buildHref ↔ matchLocation round trips", () => {
  function roundtrip(match: RouteMatch): RouteMatch {
    const href = buildHref(match);
    const [pathname, search] = href.split("?");
    return matchLocation(pathname ?? "/", search ? `?${search}` : "");
  }

  test("channel round-trips with thread stack and pinned flag", () => {
    const m: RouteMatch = {
      id: "channel",
      params: { channelId: "space-a-general" },
      search: { thread: ["A", "B", "C"], pinned: true },
    };
    expect(roundtrip(m)).toEqual(m);
  });

  test("ids containing URL-reserved characters round-trip", () => {
    const m: RouteMatch = {
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: ["id with spaces", "id/with/slashes"], pinned: false },
    };
    expect(roundtrip(m)).toEqual(m);
  });

  test("ids containing literal commas round-trip", () => {
    const m: RouteMatch = {
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: ["before,after", "plain"], pinned: false },
    };
    expect(buildHref(m)).toContain("%2C");
    expect(roundtrip(m)).toEqual(m);
  });

  test("ids containing literal percent signs round-trip", () => {
    const m: RouteMatch = {
      id: "channel",
      params: { channelId: "room-id-1" },
      search: { thread: ["100%", "a%20b"], pinned: false },
    };
    expect(roundtrip(m)).toEqual(m);
  });

  test("dm with XMPP localpart specials round-trips", () => {
    const m: RouteMatch = {
      id: "dm",
      params: { username: "first.last_user+test" },
      search: { thread: ["root"] },
    };
    expect(roundtrip(m)).toEqual(m);
  });

  test("channelExtension round-trips fully", () => {
    const m: RouteMatch = {
      id: "channelExtension",
      params: { channelId: "ch", pluginId: "link-board", routeId: "saved-links" },
      search: { thread: ["root,1", "child%2"], pinned: true },
    };
    expect(roundtrip(m)).toEqual(m);
  });
});

describe("routes.<route>.href parity with manual hrefs", () => {
  test("channel.href and channelExtension.href produce buildHref output", () => {
    const channelMatch = routes.channel.match({
      params: { channelId: "room-id-1" },
      search: { thread: ["a"], pinned: true },
    });
    expect(routes.channel.href({ params: channelMatch.params, search: channelMatch.search })).toBe(
      buildHref(channelMatch),
    );

    const extMatch = routes.channelExtension.match({
      params: { channelId: "x", pluginId: "p", routeId: "r" },
      search: { thread: [], pinned: false },
    });
    expect(
      routes.channelExtension.href({ params: extMatch.params, search: extMatch.search }),
    ).toBe(buildHref(extMatch));
  });
});

describe("navigate", () => {
  let pushed: Array<{ state: unknown; url: string }>;
  let replaced: Array<{ state: unknown; url: string }>;
  const hadWindow = "window" in globalThis;
  const previousWindow = hadWindow ? (globalThis as { window?: unknown }).window : undefined;

  beforeEach(() => {
    pushed = [];
    replaced = [];
    const fakeLocation = { pathname: "/", search: "" };
    const fakeWindow = {
      location: fakeLocation,
      history: {
        pushState(state: unknown, _title: string, url: string) {
          pushed.push({ state, url });
          const [path, search] = url.split("?");
          fakeLocation.pathname = path ?? "/";
          fakeLocation.search = search ? `?${search}` : "";
        },
        replaceState(state: unknown, _title: string, url: string) {
          replaced.push({ state, url });
          const [path, search] = url.split("?");
          fakeLocation.pathname = path ?? "/";
          fakeLocation.search = search ? `?${search}` : "";
        },
      },
    };
    (globalThis as { window?: unknown }).window = fakeWindow;
  });

  afterEach(() => {
    if (hadWindow) {
      (globalThis as { window?: unknown }).window = previousWindow;
    } else {
      delete (globalThis as { window?: unknown }).window;
    }
  });

  test("pushes /threads", () => {
    navigate({ id: "threads" });
    expect(pushed).toEqual([{ state: { waddleRouteId: "threads" }, url: "/threads" }]);
  });

  test("is a no-op when already on the target URL", () => {
    const w = (globalThis as unknown as { window: { location: { pathname: string } } }).window;
    w.location.pathname = "/threads";
    navigate({ id: "threads" });
    expect(pushed).toHaveLength(0);
  });

  test("settings carries origin in history.state", () => {
    navigate({ id: "settings", origin: "app" });
    expect(pushed).toEqual([
      { state: { waddleRouteId: "settings", origin: "app" }, url: "/settings" },
    ]);
  });

  test("replace mode uses replaceState", () => {
    navigate({ id: "threads" }, { replace: true });
    expect(pushed).toHaveLength(0);
    expect(replaced).toHaveLength(1);
  });
});

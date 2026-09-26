import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { buildHref, matchLocation, navigate, settleIslandMatch, type RouteMatch } from "../src/router";
import { currentMatch } from "../src/router/use-route-match";

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

  test("parses /rooms and /members as community pages", () => {
    expect(matchLocation("/rooms", "")).toEqual({ id: "rooms" });
    expect(matchLocation("/members", "")).toEqual({ id: "members" });
  });

  test("parses /feed, /stories, /events as community-surface routes", () => {
    expect(matchLocation("/feed", "")).toEqual({ id: "feed" });
    expect(matchLocation("/stories", "")).toEqual({ id: "stories" });
    expect(matchLocation("/events", "")).toEqual({ id: "events" });
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
    const m = matchLocation("/dm/first.last_user%2Btest%40example.com", "");
    expect(m).toEqual({
      id: "dm",
      params: { peerJid: "first.last_user+test@example.com" },
      search: { thread: [], pinned: false },
    });
  });

  test("parses dm with thread stack and pinned flag", () => {
    const m = matchLocation("/dm/first.last_user%2Btest%40example.com", "?thread=root&pinned=1");
    expect(m).toEqual({
      id: "dm",
      params: { peerJid: "first.last_user+test@example.com" },
      search: { thread: ["root"], pinned: true },
    });
  });

  test.each(["bob@elsewhere.example", "chat@muc.example.com/Nick/Device", "room@muc.example.com/Nick ?#%"])("DM links preserve the full address %s", (peerJid) => {
    const match = { id: "dm" as const, params: { peerJid }, search: { thread: ["root"], pinned: true, ...(peerJid.includes("/") ? { scope: "occupant" as const } : {}) } };
    const url = new URL(buildHref(match), "https://example.com");
    expect(matchLocation(url.pathname, url.search)).toEqual(match);
  });

  test.each(["chat", "chat%40", "chat%40example.com%40example.com", "%E0%A4%A"])("does not turn an invalid DM address %s into an account", (segment) => {
    expect(matchLocation(`/dm/${segment}`)).toEqual({ id: "home" });
  });

  test("parses /dm as the DM list route (distinct from /dm/:peerJid)", () => {
    expect(matchLocation("/dm", "")).toEqual({ id: "dmList" });
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

  test("extra path segments after a known admin panel are ignored", () => {
    expect(matchLocation("/admin/users/anything/here", "")).toEqual({
      id: "admin",
      params: { panel: "users" },
    });
  });

  test("falls back to home for /r/ with no channel id", () => {
    expect(matchLocation("/r/", "")).toEqual({ id: "home" });
  });

  test("falls back to home for /dm/ with no peer address", () => {
    expect(matchLocation("/dm/", "")).toEqual({ id: "home" });
  });

  test("falls back to home for /r/foo/x/ with missing extension params", () => {
    expect(matchLocation("/r/foo/x/", "")).toEqual({ id: "home" });
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

  test("rooms / members", () => {
    expect(buildHref({ id: "rooms" })).toBe("/rooms");
    expect(buildHref({ id: "members" })).toBe("/members");
  });

  test("feed / stories alias / events", () => {
    expect(buildHref({ id: "feed" })).toBe("/feed");
    expect(buildHref({ id: "stories" })).toBe("/stories");
    expect(buildHref({ id: "events" })).toBe("/events");
  });

  test("dmList", () => {
    expect(buildHref({ id: "dmList" })).toBe("/dm");
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
      params: { peerJid: "first.last_user+test@example.com" },
      search: { thread: [] },
    });
    expect(href).toBe("/dm/first.last_user%2Btest%40example.com");
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
      params: { peerJid: "first.last_user+test@example.com" },
      search: { thread: ["root"], pinned: false },
    };
    expect(roundtrip(m)).toEqual(m);
  });

  test("dm with pinned flag round-trips", () => {
    const m: RouteMatch = {
      id: "dm",
      params: { peerJid: "first.last_user+test@example.com" },
      search: { thread: ["root"], pinned: true },
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

  test("settleIslandMatch leaves a matching island alone", () => {
    const w = (globalThis as unknown as { window: { location: { pathname: string } } }).window;
    w.location.pathname = "/dm/alice%40example.com";
    currentMatch.value = matchLocation(w.location.pathname, "");

    settleIslandMatch("dm");

    expect(replaced).toHaveLength(0);
    expect(pushed).toHaveLength(0);
    expect(currentMatch.value.id).toBe("dm");
  });

  test("settleIslandMatch replaces a URL the route parser rejected with the match's href", () => {
    // Astro routes `/dm/alice` to the dm page island, but the router
    // needs a full address so the match fell back to home: the island
    // must not throw, it moves the URL to where the match is.
    const w = (globalThis as unknown as { window: { location: { pathname: string } } }).window;
    w.location.pathname = "/dm/alice";
    currentMatch.value = matchLocation(w.location.pathname, "");
    expect(currentMatch.value).toEqual({ id: "home" });

    settleIslandMatch("dm");

    expect(pushed).toHaveLength(0);
    expect(replaced).toEqual([{ state: { waddleRouteId: "home" }, url: "/" }]);
    expect(w.location.pathname).toBe("/");
    expect(currentMatch.value).toEqual({ id: "home" });
  });

  test("stories compatibility route can canonicalize to feed with replaceState", () => {
    const w = (globalThis as unknown as { window: { location: { pathname: string } } }).window;
    w.location.pathname = "/stories";

    navigate({ id: "feed" }, { replace: true });

    expect(pushed).toHaveLength(0);
    expect(replaced).toEqual([{ state: { waddleRouteId: "feed" }, url: "/feed" }]);
  });
});

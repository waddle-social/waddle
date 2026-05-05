import type { ChannelSummary } from "@/lib/chat-types";

interface RouteState {
  page: "dashboard" | "chat" | "settings" | "extension";
  channelSlug: string | null;
  dmUsername: string | null;
  extensionPluginId: string | null;
  extensionRouteId: string | null;
  /** XEP-0201 thread stack from `?thread=rootId,childId,...`. Empty = panel closed. */
  threadStack: string[];
}

const SETTINGS_PATH = "/settings";

function parseThreadStack(search: string | null | undefined): string[] {
  if (!search) return [];
  // Match the raw `thread` value directly: `URLSearchParams.get()` already
  // decodes percent-encoding, and we'd then decode again — so ids with `%`
  // or `,` (which we encode as `%2C` to survive splitting) get mangled.
  const query = search.startsWith("?") ? search.slice(1) : search;
  const match = /(?:^|&)thread=([^&]*)/.exec(query);
  const raw = match?.[1];
  if (!raw) return [];
  return raw
    .split(",")
    .map((s) => {
      const trimmed = s.trim();
      try {
        return decodeURIComponent(trimmed);
      } catch {
        return trimmed;
      }
    })
    .filter((s) => s.length > 0);
}

function buildSearch(threadStack: string[] | undefined): string {
  if (!threadStack || threadStack.length === 0) return "";
  const encoded = threadStack.map((id) => encodeURIComponent(id)).join(",");
  return `?thread=${encoded}`;
}

// Parses canonical Waddle app routes.
export function parseChatLocation(pathname: string, search?: string): RouteState {
  const segments = pathname.split("/").filter(Boolean);
  const resolvedSearch = search ?? (typeof window !== "undefined" ? window.location.search : "");
  const threadStack = parseThreadStack(resolvedSearch);
  if (pathname === "/" || segments.length === 0) {
    return {
      page: "dashboard",
      channelSlug: null,
      dmUsername: null,
      extensionPluginId: null,
      extensionRouteId: null,
      threadStack: [],
    };
  }
  if (pathname === SETTINGS_PATH) {
    return {
      page: "settings",
      channelSlug: null,
      dmUsername: null,
      extensionPluginId: null,
      extensionRouteId: null,
      threadStack: [],
    };
  }
  if (segments[0] === "r") {
    if (segments[2] === "x" && segments[3] && segments[4]) {
      return {
        page: "extension",
        channelSlug: segments[1] ? decodeURIComponent(segments[1]) : null,
        dmUsername: null,
        extensionPluginId: decodeURIComponent(segments[3]),
        extensionRouteId: decodeURIComponent(segments[4]),
        threadStack: [],
      };
    }
    return {
      page: "chat",
      channelSlug: segments[1] ? decodeURIComponent(segments[1]) : null,
      dmUsername: null,
      extensionPluginId: null,
      extensionRouteId: null,
      threadStack,
    };
  }
  if (segments[0] === "dm") {
    return {
      page: "chat",
      channelSlug: null,
      dmUsername: segments[1] ? decodeURIComponent(segments[1]) : null,
      extensionPluginId: null,
      extensionRouteId: null,
      threadStack,
    };
  }
  return {
    page: "dashboard",
    channelSlug: null,
    dmUsername: null,
    extensionPluginId: null,
    extensionRouteId: null,
    threadStack: [],
  };
}

export function resolveChannelBySlug(slug: string, channels: ChannelSummary[]): ChannelSummary | undefined {
  return channels.find((c) => c.id === slug);
}

export function shouldLoadWaddleStructureForRoute(
  route: RouteState,
  channelCount: number,
): boolean {
  return (route.page === "dashboard" || route.page === "extension" || (route.page === "chat" && !route.dmUsername)) && channelCount === 0;
}

export function buildChannelPath(
  channel: ChannelSummary | null,
  threadStack?: string[],
): string {
  const search = buildSearch(threadStack);
  return channel ? `/r/${encodeURIComponent(channel.id)}${search}` : "/";
}

export function buildChannelExtensionPath(
  channel: ChannelSummary | null,
  pluginId: string | null | undefined,
  routeId: string | null | undefined,
): string {
  if (!channel || !pluginId || !routeId) return "/";
  return `/r/${encodeURIComponent(channel.id)}/x/${encodeURIComponent(pluginId)}/${encodeURIComponent(routeId)}`;
}

export function buildDirectMessagePath(username: string | null, threadStack?: string[]): string {
  const search = buildSearch(threadStack);
  return username ? `/dm/${encodeURIComponent(username)}${search}` : "/";
}

export function buildChatSettingsPath(): string {
  return SETTINGS_PATH;
}

export function pushChannelRoute(
  channel: ChannelSummary | null,
  threadStack?: string[],
) {
  const path = buildChannelPath(channel, threadStack);
  const current = window.location.pathname + window.location.search;
  if (current !== path) {
    window.history.pushState({ waddlePage: "chat" }, "", path);
  }
}

export function pushDirectMessageRoute(username: string | null, threadStack?: string[]) {
  const path = buildDirectMessagePath(username, threadStack);
  const current = window.location.pathname + window.location.search;
  if (current !== path) {
    window.history.pushState({ waddlePage: "chat" }, "", path);
  }
}

export function pushChatSettingsRoute(origin: "app" | "direct" = "app") {
  const current = window.location.pathname + window.location.search;
  if (current !== SETTINGS_PATH) {
    window.history.pushState({ waddlePage: "settings", origin }, "", SETTINGS_PATH);
  }
}

export function pushChannelExtensionRoute(
  channel: ChannelSummary | null,
  pluginId: string | null | undefined,
  routeId: string | null | undefined,
) {
  const path = buildChannelExtensionPath(channel, pluginId, routeId);
  const current = window.location.pathname + window.location.search;
  if (current !== path) {
    window.history.pushState({ waddlePage: "extension" }, "", path);
  }
}

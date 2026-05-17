import type { ChannelSummary } from "@/lib/chat-types";

interface RouteState {
  page: "dashboard" | "chat" | "settings" | "extension" | "admin" | "threads";
  channelSlug: string | null;
  dmUsername: string | null;
  extensionPluginId: string | null;
  extensionRouteId: string | null;
  /** XEP-0201 thread stack from `?thread=rootId,childId,...`. Empty = panel closed. */
  threadStack: string[];
  /** #414: pinned-messages right-rail panel (`?pinned=1`). */
  pinnedPanelOpen: boolean;
  /**
   * Active admin panel slug when `page === "admin"`. V1 only
   * implements "users"; the other slugs (spaces / audit / push-health
   * / settings) parse to the same value so the layout can render a
   * stub-mode placeholder without a separate route table.
   */
  adminPanel: AdminPanel | null;
}

/** Slugs the admin layout understands. V1 only implements `users`. */
export type AdminPanel = "users" | "spaces" | "audit" | "push-health" | "settings";

const SETTINGS_PATH = "/settings";
const ADMIN_PATH_PREFIX = "/admin";
const DEFAULT_ADMIN_PANEL: AdminPanel = "users";
const THREADS_PATH = "/threads";

function parseAdminPanel(slug: string | undefined): AdminPanel | null {
  switch (slug) {
    case undefined:
    case "":
      return DEFAULT_ADMIN_PANEL;
    case "users":
    case "spaces":
    case "audit":
    case "push-health":
    case "settings":
      return slug;
    default:
      return null;
  }
}

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

function parsePinnedFlag(search: string | null | undefined): boolean {
  if (!search) return false;
  const query = search.startsWith("?") ? search.slice(1) : search;
  return /(?:^|&)pinned=1(?:&|$)/.test(query);
}

function buildSearch(threadStack: string[] | undefined, pinnedPanelOpen?: boolean): string {
  const parts: string[] = [];
  if (threadStack && threadStack.length > 0) {
    const encoded = threadStack.map((id) => encodeURIComponent(id)).join(",");
    parts.push(`thread=${encoded}`);
  }
  if (pinnedPanelOpen) parts.push("pinned=1");
  return parts.length > 0 ? `?${parts.join("&")}` : "";
}

// Parses canonical Waddle app routes.
export function parseChatLocation(pathname: string, search?: string): RouteState {
  const segments = pathname.split("/").filter(Boolean);
  const resolvedSearch = search ?? (typeof window !== "undefined" ? window.location.search : "");
  const threadStack = parseThreadStack(resolvedSearch);
  const pinnedPanelOpen = parsePinnedFlag(resolvedSearch);
  if (pathname === "/" || segments.length === 0) {
    return {
      page: "dashboard",
      channelSlug: null,
      dmUsername: null,
      extensionPluginId: null,
      extensionRouteId: null,
      threadStack: [],
      pinnedPanelOpen: false,
      adminPanel: null,
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
      pinnedPanelOpen: false,
      adminPanel: null,
    };
  }
  if (segments[0] === "admin") {
    const panel = parseAdminPanel(segments[1]);
    return {
      page: "admin",
      channelSlug: null,
      dmUsername: null,
      extensionPluginId: null,
      extensionRouteId: null,
      threadStack: [],
      pinnedPanelOpen: false,
      adminPanel: panel,
    };
  }
  if (pathname === THREADS_PATH) {
    return {
      page: "threads",
      channelSlug: null,
      dmUsername: null,
      extensionPluginId: null,
      extensionRouteId: null,
      threadStack: [],
      pinnedPanelOpen: false,
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
        threadStack,
        pinnedPanelOpen,
        adminPanel: null,
      };
    }
    return {
      page: "chat",
      channelSlug: segments[1] ? decodeURIComponent(segments[1]) : null,
      dmUsername: null,
      extensionPluginId: null,
      extensionRouteId: null,
      threadStack,
      pinnedPanelOpen,
      adminPanel: null,
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
      pinnedPanelOpen: false,
      adminPanel: null,
    };
  }
  return {
    page: "dashboard",
    channelSlug: null,
    dmUsername: null,
    extensionPluginId: null,
    extensionRouteId: null,
    threadStack: [],
    pinnedPanelOpen: false,
    adminPanel: null,
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
  pinnedPanelOpen?: boolean,
): string {
  const search = buildSearch(threadStack, pinnedPanelOpen);
  return channel ? `/r/${encodeURIComponent(channel.id)}${search}` : "/";
}

export function buildChannelExtensionPath(
  channel: ChannelSummary | null,
  pluginId: string | null | undefined,
  routeId: string | null | undefined,
  threadStack?: string[],
  pinnedPanelOpen?: boolean,
): string {
  if (!channel || !pluginId || !routeId) return "/";
  const search = buildSearch(threadStack, pinnedPanelOpen);
  return `/r/${encodeURIComponent(channel.id)}/x/${encodeURIComponent(pluginId)}/${encodeURIComponent(routeId)}${search}`;
}

export function buildDirectMessagePath(username: string | null, threadStack?: string[]): string {
  const search = buildSearch(threadStack);
  return username ? `/dm/${encodeURIComponent(username)}${search}` : "/";
}

export function buildChatSettingsPath(): string {
  return SETTINGS_PATH;
}

export function buildAdminPath(panel: AdminPanel = DEFAULT_ADMIN_PANEL): string {
  return `${ADMIN_PATH_PREFIX}/${panel}`;
}

export function pushThreadsRoute() {
  const current = window.location.pathname + window.location.search;
  if (current !== THREADS_PATH) {
    window.history.pushState({ waddlePage: "threads" }, "", THREADS_PATH);
  }
}

export function pushChannelRoute(
  channel: ChannelSummary | null,
  threadStack?: string[],
  pinnedPanelOpen?: boolean,
) {
  const path = buildChannelPath(channel, threadStack, pinnedPanelOpen);
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
  threadStack?: string[],
  pinnedPanelOpen?: boolean,
) {
  const path = buildChannelExtensionPath(channel, pluginId, routeId, threadStack, pinnedPanelOpen);
  const current = window.location.pathname + window.location.search;
  if (current !== path) {
    window.history.pushState({ waddlePage: "extension" }, "", path);
  }
}

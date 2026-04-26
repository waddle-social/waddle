import type { ChannelSummary } from "@/lib/chat-types";

interface RouteState {
  page: "dashboard" | "chat" | "settings";
  channelSlug: string | null;
  dmUsername: string | null;
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
export function parseRoute(pathname: string, search?: string): RouteState {
  const segments = pathname.split("/").filter(Boolean);
  const resolvedSearch = search ?? (typeof window !== "undefined" ? window.location.search : "");
  const threadStack = parseThreadStack(resolvedSearch);
  if (pathname === "/" || segments.length === 0) {
    return {
      page: "dashboard",
      channelSlug: null,
      dmUsername: null,
      threadStack: [],
    };
  }
  if (pathname === SETTINGS_PATH) {
    return {
      page: "settings",
      channelSlug: null,
      dmUsername: null,
      threadStack: [],
    };
  }
  if (segments[0] === "r") {
    return {
      page: "chat",
      channelSlug: segments[1] ? decodeURIComponent(segments[1]) : null,
      dmUsername: null,
      threadStack,
    };
  }
  if (segments[0] === "dm") {
    return {
      page: "chat",
      channelSlug: null,
      dmUsername: segments[1] ? decodeURIComponent(segments[1]) : null,
      threadStack,
    };
  }
  return {
    page: "dashboard",
    channelSlug: null,
    dmUsername: null,
    threadStack: [],
  };
}

export function resolveChannel(slug: string, channels: ChannelSummary[]): ChannelSummary | undefined {
  return channels.find((c) => c.id === slug);
}

export function shouldLoadStructureForRoute(
  route: RouteState,
  channelCount: number,
): boolean {
  return (route.page === "dashboard" || (route.page === "chat" && !route.dmUsername)) && channelCount === 0;
}

export function buildPath(
  channel: ChannelSummary | null,
  threadStack?: string[],
): string {
  const search = buildSearch(threadStack);
  return channel ? `/r/${encodeURIComponent(channel.id)}${search}` : "/";
}

export function buildDmPath(username: string | null, threadStack?: string[]): string {
  const search = buildSearch(threadStack);
  return username ? `/dm/${encodeURIComponent(username)}${search}` : "/";
}

export function buildSettingsPath(): string {
  return SETTINGS_PATH;
}

export function pushRoute(
  channel: ChannelSummary | null,
  threadStack?: string[],
) {
  const path = buildPath(channel, threadStack);
  const current = window.location.pathname + window.location.search;
  if (current !== path) {
    window.history.pushState({ waddlePage: "chat" }, "", path);
  }
}

export function pushDmRoute(username: string | null, threadStack?: string[]) {
  const path = buildDmPath(username, threadStack);
  const current = window.location.pathname + window.location.search;
  if (current !== path) {
    window.history.pushState({ waddlePage: "chat" }, "", path);
  }
}

export function pushSettingsRoute(origin: "app" | "direct" = "app") {
  const current = window.location.pathname + window.location.search;
  if (current !== SETTINGS_PATH) {
    window.history.pushState({ waddlePage: "settings", origin }, "", SETTINGS_PATH);
  }
}

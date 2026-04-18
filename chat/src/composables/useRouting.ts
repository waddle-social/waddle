import type { WaddleSummary, ChannelSummary } from "@/lib/waddle-api";

export interface RouteState {
  page: "chat" | "settings";
  waddleSlug: string | null;
  channelSlug: string | null;
  dmUsername: string | null;
  /** XEP-0201 thread stack from `?thread=rootId,childId,...`. Empty = panel closed. */
  threadStack: string[];
}

const SETTINGS_PATH = "/_settings";

function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
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

function buildSearch(threadStack: string[] | undefined): string {
  if (!threadStack || threadStack.length === 0) return "";
  const encoded = threadStack.map((id) => encodeURIComponent(id)).join(",");
  return `?thread=${encoded}`;
}

// Parses /{waddleSlug}/{channelSlug}[?thread=a,b,c] from URL.
export function parseRoute(pathname: string, search?: string): RouteState {
  const segments = pathname.split("/").filter(Boolean);
  const resolvedSearch = search ?? (typeof window !== "undefined" ? window.location.search : "");
  const threadStack = parseThreadStack(resolvedSearch);
  if (pathname === SETTINGS_PATH) {
    return {
      page: "settings",
      waddleSlug: null,
      channelSlug: null,
      dmUsername: null,
      threadStack: [],
    };
  }
  if (segments[0] === "dm") {
    return {
      page: "chat",
      waddleSlug: null,
      channelSlug: null,
      dmUsername: segments[1] ? decodeURIComponent(segments[1]) : null,
      threadStack,
    };
  }
  return {
    page: "chat",
    waddleSlug: segments[0] ? decodeURIComponent(segments[0]) : null,
    channelSlug: segments[1] ? decodeURIComponent(segments[1]) : null,
    dmUsername: null,
    threadStack,
  };
}

export function resolveWaddle(slug: string, waddles: WaddleSummary[]): WaddleSummary | undefined {
  const lower = slug.toLowerCase();
  return (
    waddles.find((w) => slugify(w.name) === lower) ??
    waddles.find((w) => w.name.toLowerCase() === lower) ??
    waddles.find((w) => w.id === slug)
  );
}

export function resolveChannel(slug: string, channels: ChannelSummary[]): ChannelSummary | undefined {
  const lower = slug.toLowerCase();
  return (
    channels.find((c) => slugify(c.name) === lower) ??
    channels.find((c) => c.name.toLowerCase() === lower) ??
    channels.find((c) => c.id === slug)
  );
}

export function buildPath(
  waddle: WaddleSummary | null,
  channel: ChannelSummary | null,
  threadStack?: string[],
): string {
  const search = buildSearch(threadStack);
  if (!waddle) return `/${search}`;
  const wSlug = encodeURIComponent(slugify(waddle.name));
  if (channel) {
    return `/${wSlug}/${encodeURIComponent(slugify(channel.name))}${search}`;
  }
  return `/${wSlug}${search}`;
}

export function buildDmPath(username: string | null, threadStack?: string[]): string {
  const search = buildSearch(threadStack);
  return username ? `/dm/${encodeURIComponent(slugify(username))}${search}` : `/${search}`;
}

export function buildSettingsPath(): string {
  return SETTINGS_PATH;
}

export function pushRoute(
  waddle: WaddleSummary | null,
  channel: ChannelSummary | null,
  threadStack?: string[],
) {
  const path = buildPath(waddle, channel, threadStack);
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

import type { WaddleSummary, ChannelSummary } from "@/lib/waddle-api";

export interface RouteState {
  waddleSlug: string | null;
  channelSlug: string | null;
  dmUsername: string | null;
}

function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}

// Parses /{waddleSlug}/{channelSlug} from pathname segments
export function parseRoute(pathname: string): RouteState {
  const segments = pathname.split("/").filter(Boolean);
  if (segments[0] === "dm") {
    return {
      waddleSlug: null,
      channelSlug: null,
      dmUsername: segments[1] ? decodeURIComponent(segments[1]) : null,
    };
  }
  return {
    waddleSlug: segments[0] ? decodeURIComponent(segments[0]) : null,
    channelSlug: segments[1] ? decodeURIComponent(segments[1]) : null,
    dmUsername: null,
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
): string {
  if (!waddle) return "/";
  const wSlug = encodeURIComponent(slugify(waddle.name));
  if (channel) {
    return `/${wSlug}/${encodeURIComponent(slugify(channel.name))}`;
  }
  return `/${wSlug}`;
}

export function pushRoute(waddle: WaddleSummary | null, channel: ChannelSummary | null) {
  const path = buildPath(waddle, channel);
  if (window.location.pathname !== path) {
    window.history.pushState(null, "", path);
  }
}

export function pushDmRoute(username: string | null) {
  const path = username ? `/dm/${encodeURIComponent(slugify(username))}` : "/";
  if (window.location.pathname !== path) {
    window.history.pushState(null, "", path);
  }
}

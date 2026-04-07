import type { WaddleSummary, ChannelSummary } from "@/lib/waddle-api";

export interface RouteState {
  waddleSlug: string | null;
  channelSlug: string | null;
}

function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}

// Parses /w/{waddleSlug}/c/{channelSlug} from pathname segments
export function parseRoute(pathname: string): RouteState {
  const segments = pathname.split("/").filter(Boolean);
  let waddleSlug: string | null = null;
  let channelSlug: string | null = null;

  for (let i = 0; i < segments.length - 1; i++) {
    if (segments[i] === "w") {
      waddleSlug = decodeURIComponent(segments[i + 1]!);
    } else if (segments[i] === "c") {
      channelSlug = decodeURIComponent(segments[i + 1]!);
    }
  }

  return { waddleSlug, channelSlug };
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
  let path = `/w/${encodeURIComponent(slugify(waddle.name))}`;
  if (channel) {
    path += `/c/${encodeURIComponent(slugify(channel.name))}`;
  }
  return path;
}

export function pushRoute(waddle: WaddleSummary | null, channel: ChannelSummary | null) {
  const path = buildPath(waddle, channel);
  if (window.location.pathname !== path) {
    window.history.pushState(null, "", path);
  }
}

import type { ChannelSummary } from "@/lib/chat-types";
import type { RouteMatch } from "@/router";

// Channel lookup is by globally-unique id (the same id Astro receives via
// `[room]`), not by display name — different spaces can have channels
// with the same human name.
export function resolveChannelBySlug(
  slug: string,
  channels: ChannelSummary[],
): ChannelSummary | undefined {
  return channels.find((c) => c.id === slug);
}

// On a cold URL hit (no waddle topology hydrated yet), the home dashboard
// and any channel-targeting route both need the channel list before they
// can render anything meaningful. DM routes don't depend on the channel
// list and are excluded.
export function shouldLoadStructureForMatch(
  match: RouteMatch,
  channelCount: number,
): boolean {
  if (channelCount > 0) return false;
  return (
    match.id === "home" || match.id === "channel" || match.id === "channelExtension"
  );
}

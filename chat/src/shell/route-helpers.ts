import type { ChannelSummary } from "@/lib/chat-types";

// Channel lookup is by globally-unique id (the same id Astro receives via
// `[room]`), not by display name — different spaces can have channels
// with the same human name.
export function resolveChannelBySlug(
  slug: string,
  channels: ChannelSummary[],
): ChannelSummary | undefined {
  return channels.find((c) => c.id === slug);
}

/**
 * `/dm/:username` shares the same slug space as channel ids. Production
 * traffic to `/dm/chat` opened a 1:1 with `chat@waddle.social` while the
 * community room is `chat@muc.waddle.social`. Treat a DM username that
 * matches a discovered room id as that room, not as a user.
 *
 * This is a URL-slug heuristic only. A full user-domain JID whose node
 * matches a channel id is still a 1:1 — see `resolveThreadEntryTarget`
 * and #917. Comparison is case-insensitive: channel ids are lowercase
 * and RFC 7622 localparts are case-mapped.
 */
export function resolveRoomByDmUsername(
  username: string,
  channels: ChannelSummary[],
): ChannelSummary | undefined {
  const slug = username.replace(/^@/, "").trim().toLowerCase();
  if (!slug) return undefined;
  return channels.find((channel) => channel.id.toLowerCase() === slug);
}

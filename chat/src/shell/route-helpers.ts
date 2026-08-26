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
 */
export function resolveRoomByDmUsername(
  username: string,
  channels: ChannelSummary[],
): ChannelSummary | undefined {
  const slug = username.replace(/^@/, "").trim();
  if (!slug) return undefined;
  return resolveChannelBySlug(slug, channels);
}

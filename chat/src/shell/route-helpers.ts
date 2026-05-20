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

import { hasConflictingRoomAddresses } from "@/lib/xmpp/room-id-bindings";
import type { ChannelSummary } from "@/lib/chat-types";

// Legacy routes carry a local ID, not a room authority. Reject an ambiguous
// snapshot rather than choosing its first room. Display names are not IDs.
export function resolveChannelBySlug(
  slug: string,
  channels: ChannelSummary[],
): ChannelSummary | undefined {
  if (hasConflictingRoomAddresses(slug, channels)) return undefined;
  return channels.find((c) => c.id === slug);
}

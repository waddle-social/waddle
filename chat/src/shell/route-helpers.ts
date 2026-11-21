import { hasConflictingRoomAddresses } from "@/lib/xmpp/room-id-bindings";
import { qualifiedRoomKey } from "@/lib/xmpp/qualified-identity";
import type { ChannelSummary } from "@/lib/chat-types";

// A route slug is either a qualified room address (a bare JID, which names
// exactly one room) or a legacy local ID, which does not name a room
// authority. Reject an ambiguous legacy ID rather than choosing the first
// room. Display names are not IDs.
export function resolveChannelBySlug(
  slug: string,
  channels: ChannelSummary[],
): ChannelSummary | undefined {
  if (slug.includes("@")) {
    const wanted = qualifiedRoomKey(slug);
    const matches = channels.filter((c) => c.jid && qualifiedRoomKey(c.jid) === wanted);
    return matches.length === 1 ? matches[0] : undefined;
  }
  if (hasConflictingRoomAddresses(slug, channels)) return undefined;
  return channels.find((c) => c.id === slug);
}

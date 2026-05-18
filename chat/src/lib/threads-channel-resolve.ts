// Resolve a thread entry's room JID back to the channel-id slug the
// controller's `onSelectThread(channelId, threadId)` expects.
//
// Waddle thread entries (XEP `urn:waddle:threads:0`) carry the room
// JID — e.g. `general@muc.waddle.example` — but the controller works
// in terms of the channel-id slug (`general`). We resolve in the most
// specific way available and fall back to the local-part split which
// is always correct for managed Waddle rooms.

import { barePeerJid } from "@/lib/xmpp/jid";

interface ChannelLike {
  id: string;
  jid?: string;
}

/**
 * Returns the channel-id slug for a given room JID, or `null` if the
 * JID is unusable (empty/no local-part).
 *
 * Resolution order:
 *  1. Exact `channel.jid` match (bare-on-bare).
 *  2. Local-part of the JID — managed rooms encode the channel-id as
 *     the JID's node, so this is the authoritative fallback.
 */
export function resolveChannelIdForRoomJid(
  roomJid: string,
  channels: readonly ChannelLike[],
): string | null {
  const bare = barePeerJid(roomJid);
  if (!bare) return null;
  const matched = channels.find((c) => c.jid && barePeerJid(c.jid) === bare);
  if (matched) return matched.id;
  const node = bare.split("@")[0] ?? "";
  return node.length > 0 ? node : null;
}

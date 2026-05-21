import type { ChannelSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, roomBareJidFor } from "@/lib/xmpp-client";
import { parseManagedRoomBareJid } from "@/lib/xmpp/jid";

export function roomJidForChannelSummary(
  session: WaddleSession,
  channel: Pick<ChannelSummary, "id" | "jid">,
): string {
  return channel.jid ? barePeerJid(channel.jid) : roomBareJidFor(session, channel.id);
}

export function roomJidForChannelId(
  session: WaddleSession,
  channels: readonly Pick<ChannelSummary, "id" | "jid">[],
  channelId: string,
): string {
  const channel = channels.find((c) => c.id === channelId);
  return roomJidForChannelSummary(session, channel ?? { id: channelId });
}

/**
 * Reverse of `roomJidForChannelId`. Resolves a bare MUC room JID
 * back to the local channel id it represents, or `null` when no
 * known channel owns the room.
 *
 * Lookup order:
 * 1. Explicit `channel.jid` match — covers federated rooms whose
 *    JID does not follow the `{channelId}@muc.{domain}` convention.
 * 2. `parseManagedRoomBareJid` fallback — for managed rooms minted
 *    by this server, the localpart IS the channel id, so a
 *    channel-list miss still resolves as long as the channel exists.
 */
export function channelIdForRoomJid(
  channels: readonly Pick<ChannelSummary, "id" | "jid">[],
  roomJid: string,
): string | null {
  const normalized = barePeerJid(roomJid);
  if (!normalized) return null;
  const byJid = channels.find(
    (c) => c.jid !== undefined && c.jid !== null && barePeerJid(c.jid) === normalized,
  );
  if (byJid) return byJid.id;
  const parsed = parseManagedRoomBareJid(normalized);
  if (!parsed) return null;
  const byManagedId = channels.find((c) => c.id === parsed.channelId);
  return byManagedId ? byManagedId.id : null;
}

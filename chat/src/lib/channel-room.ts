import type { ChannelSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, roomBareJidFor } from "@/lib/xmpp-client";

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

import type { ChannelSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, parseManagedRoomBareJid, roomBareJidFor } from "@/lib/xmpp-client";

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

export function knownChannelIdForRoomJid(
  roomJid: string,
  channels: readonly Pick<ChannelSummary, "id" | "jid">[],
  managedMucDomain?: string | null,
): string | null {
  const normalizedRoomJid = barePeerJid(roomJid).trim().toLowerCase();
  if (!normalizedRoomJid) return null;

  const explicit = channels.find((channel) =>
    channel.jid ? barePeerJid(channel.jid).trim().toLowerCase() === normalizedRoomJid : false
  );
  if (explicit) return explicit.id;

  const roomDomain = normalizedRoomJid.split("@")[1] ?? "";
  const trustedDomain = normalizeManagedMucDomain(managedMucDomain);
  if (!trustedDomain || roomDomain !== trustedDomain) return null;

  const managedRoom = parseManagedRoomBareJid(normalizedRoomJid);
  if (!managedRoom) return null;
  const managedChannelId = managedRoom.channelId.toLowerCase();
  return channels.find((channel) =>
    !channel.jid && channel.id.toLowerCase() === managedChannelId
  )?.id ?? null;
}

export function isTrustedManagedRoomJid(
  roomJid: string,
  managedMucDomain?: string | null,
): boolean {
  const normalizedRoomJid = barePeerJid(roomJid).trim().toLowerCase();
  const roomDomain = normalizedRoomJid.split("@")[1] ?? "";
  const trustedDomain = normalizeManagedMucDomain(managedMucDomain);
  return Boolean(
    roomDomain &&
    trustedDomain &&
    roomDomain === trustedDomain &&
    parseManagedRoomBareJid(normalizedRoomJid)?.channelId,
  );
}

function normalizeManagedMucDomain(managedMucDomain?: string | null): string {
  const bare = barePeerJid(managedMucDomain ?? "").trim().toLowerCase();
  if (!bare) return "";
  return bare.includes("@") ? bare.split("@")[1] ?? "" : bare;
}

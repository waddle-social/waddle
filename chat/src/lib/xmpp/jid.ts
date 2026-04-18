/** JID and room-address utilities. */
import type { WaddleSession } from "../server-auth";

export function jidDomain(jid: string): string {
  const bareJid = barePeerJid(jid);
  return bareJid.split("@")[1] ?? "localhost";
}

export function barePeerJid(fullJid: string): string {
  return fullJid.split("/")[0] ?? "";
}

export function parseManagedRoomNode(node: string): { waddleId: string; channelId: string } | null {
  const separatorIndex = node.indexOf("_");
  if (separatorIndex <= 0 || separatorIndex === node.length - 1) return null;
  return {
    waddleId: node.slice(0, separatorIndex),
    channelId: node.slice(separatorIndex + 1),
  };
}

export function parseManagedRoomBareJid(
  roomJid: string,
): { waddleId: string; channelId: string } | null {
  const node = barePeerJid(roomJid).split("@")[0] ?? "";
  if (!node) return null;
  return parseManagedRoomNode(node);
}

export function roomBareJidFor(
  session: WaddleSession,
  waddleId: string,
  channelId: string,
): string {
  return roomBareJidForAccountJid(session.jid, waddleId, channelId);
}

export function roomBareJidForAccountJid(
  jid: string,
  waddleId: string,
  channelId: string,
): string {
  return `${waddleId}_${channelId}@muc.${jidDomain(jid)}`;
}

/** JID and room-address utilities. */
import type { WaddleSession } from "../server-auth";

export function jidDomain(jid: string): string {
  const bareJid = barePeerJid(jid);
  return bareJid.split("@")[1] ?? "localhost";
}

export function barePeerJid(fullJid: string): string {
  return fullJid.split("/")[0] ?? "";
}

export function parseManagedRoomBareJid(
  roomJid: string,
): { channelId: string } | null {
  const node = barePeerJid(roomJid).split("@")[0] ?? "";
  if (!node) return null;
  return {
    channelId: node,
  };
}

export function roomBareJidFor(
  session: WaddleSession,
  channelId: string,
): string {
  return roomBareJidForAccountJid(session.jid, channelId);
}

function roomBareJidForAccountJid(
  jid: string,
  channelId: string,
): string {
  return `${channelId}@muc.${jidDomain(jid)}`;
}

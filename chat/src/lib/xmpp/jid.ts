/** JID and room-address utilities. */
import type { WaddleSession } from "../server-auth";

export function jidDomain(jid: string): string {
  const bareJid = barePeerJid(jid);
  return bareJid.split("@")[1] ?? "localhost";
}

export function barePeerJid(fullJid: string): string {
  return fullJid.split("/")[0] ?? "";
}

/** The resource part of a full JID (everything after the first `/`), or `""` for a bare JID. */
export function resourceOf(fullJid: string): string {
  const separator = fullJid.indexOf("/");
  return separator < 0 ? "" : fullJid.slice(separator + 1);
}

export function fullJidIdentityKey(fullJid?: string | null): string {
  const trimmed = fullJid?.trim() ?? "";
  if (!trimmed) return "";
  const separator = trimmed.indexOf("/");
  if (separator < 0) return trimmed.toLowerCase();
  const bare = trimmed.slice(0, separator).toLowerCase();
  const resource = trimmed.slice(separator + 1);
  return `${bare}/${resource}`;
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

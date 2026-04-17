/** JID and room-address utilities. */
import type { WaddleSession } from "../server-auth";

export function jidDomain(jid: string): string {
  return jid.split("@")[1] ?? "localhost";
}

export function barePeerJid(fullJid: string): string {
  return fullJid.split("/")[0] ?? "";
}

export function roomBareJidFor(
  session: WaddleSession,
  waddleId: string,
  channelId: string,
): string {
  return `${waddleId}_${channelId}@muc.${jidDomain(session.jid)}`;
}

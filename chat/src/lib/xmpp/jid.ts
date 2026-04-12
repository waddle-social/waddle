/** JID and room-address utilities. */
import type { WaddleSession } from "../server-auth";

export function jidDomain(jid: string): string {
  return jid.split("@")[1] ?? "localhost";
}

function websocketHost(websocketUrl: string): string {
  return new URL(websocketUrl).hostname;
}

export function roomBareJidFor(
  session: WaddleSession,
  waddleId: string,
  channelId: string,
): string {
  return `${waddleId}_${channelId}@muc.${websocketHost(session.xmpp_websocket_url)}`;
}

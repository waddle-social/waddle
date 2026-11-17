// Media posted from other servers is loaded only when the user asks.
//
// An image, video, audio file, PDF or link-preview picture is fetched from
// whatever URL the message names, so showing it tells that URL's host that
// this user read the message, and when. For messages from accounts or
// rooms on other servers that host is chosen by someone this deployment
// does not vouch for, so such media is shown as a click-to-load stand-in.
// The page's Content-Security-Policy allows any https image and media
// source (attachments come from arbitrary upload hosts), so it cannot make
// this distinction; the decision is made here, per message.

import type { InjectionKey } from "vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import { canonicalDomain, roomHost, roomHostDomain, type HomeAddresses } from "@/lib/xmpp/remote-address";

/** Decides, per message, the other server whose media waits for the user, or `null`. */
export type RemoteMediaPolicy = (message: TimelineMessage) => string | null;

export const remoteMediaPolicyKey: InjectionKey<RemoteMediaPolicy> = Symbol("remoteMediaPolicy");

function bareOf(jid: string | undefined): string {
  return (jid ?? "").split("/")[0]?.trim() ?? "";
}

function domainOf(jid: string | undefined): string | null {
  const bare = bareOf(jid);
  if (!bare) return null;
  return canonicalDomain(bare.slice(bare.lastIndexOf("@") + 1));
}

/**
 * The other server a message's media would come from the user's say-so
 * only, or `null` when it may load as before:
 * - the user's own messages always load;
 * - a message in a room on another server waits, whoever sent it;
 * - a message in a room whose host is not known yet waits too: before the
 *   home MUC service is discovered only rooms on the account domain or a
 *   known home service count as home, and the others are placed once it is;
 * - a message whose sender's real address is on another domain waits.
 *
 * A sender in a room of this deployment whose real address the room does
 * not disclose cannot be placed, and its media loads as before.
 */
export function remoteMediaSource(message: TimelineMessage, home: HomeAddresses | null): string | null {
  if (message.isSelf || !home) return null;
  const roomJid = bareOf(message.authorOccupantJid) || bareOf(message.stanzaIdBy);
  if (roomJid.includes("@") && roomHost(roomJid, home) !== "home") {
    return roomHostDomain(roomJid) ?? roomJid.slice(roomJid.lastIndexOf("@") + 1);
  }
  const accountDomain = canonicalDomain(home.accountDomain);
  const senderJid = message.authorRealJid ?? (message.authorOccupantJid ? undefined : message.authorJid);
  const senderDomain = domainOf(senderJid);
  if (senderDomain && accountDomain && senderDomain !== accountDomain) return senderDomain;
  return null;
}

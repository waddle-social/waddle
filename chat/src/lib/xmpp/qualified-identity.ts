// Qualified identities for rooms, Spaces and messages.
//
// A room localpart, a Space node name and a stanza id mean something only
// relative to the entity that issued them: two communities can each have a
// room `general`, a Space node `main` or a message id `1`. Persisted keys
// and caches use these keys instead of the bare strings.
//
// - Room: the canonical bare room JID (RFC 7622 case-folded, no resource).
// - Space: the PubSub service JID plus the opaque, case-sensitive node.
// - Message: the conversation it appears in plus the authority that issued
//   the id (XEP-0359 `by`; in a room, the room itself).
//
// Composite keys length-prefix every component so no choice of component
// contents can make two distinct identities encode to the same string.

import { bareJidKey } from "./jid";

function encodeParts(parts: readonly string[]): string {
  return parts.map((part) => `${part.length}:${part}`).join("");
}

/** Canonical identity of a room, or `""` when `roomJid` has no address. */
export function qualifiedRoomKey(roomJid: string): string {
  return bareJidKey(roomJid);
}

/** Canonical identity of a Space, or `""` when either part is missing. */
export function qualifiedSpaceKey(serviceJid: string, node: string): string {
  const service = bareJidKey(serviceJid);
  if (!service || !node) return "";
  return encodeParts(["space", service, node]);
}

/**
 * Identity of a message id within one conversation and issuing authority,
 * or `""` when any part is missing. An id whose issuer is unknown (a
 * sender-chosen origin id) has no authority and must not be qualified.
 */
export function qualifiedMessageKey(
  conversationJid: string,
  authorityJid: string,
  messageId: string,
): string {
  const conversation = bareJidKey(conversationJid);
  const authority = bareJidKey(authorityJid);
  if (!conversation || !authority || !messageId) return "";
  return encodeParts(["message", conversation, authority, messageId]);
}

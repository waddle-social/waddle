/** JID and room-address utilities. */
import type { WaddleSession } from "../server-auth";

declare const mucRoomJidBrand: unique symbol;

/**
 * A validated bare MUC room JID (`room@service`, no resource). Branded
 * so call-teardown logic can require parse-at-the-boundary instead of
 * accepting arbitrary strings; the raw bytes are preserved verbatim
 * (no case folding) so wire sends are byte-identical to the state the
 * JID came from.
 */
export type MucRoomJid = string & { readonly [mucRoomJidBrand]: true };

/**
 * Parse a raw room JID at the XMPP boundary. Rejects empty values,
 * values without an `@`, and full JIDs carrying a resource. Trims
 * surrounding whitespace but otherwise preserves the raw bytes.
 */
export function parseMucRoomJid(raw: string): MucRoomJid | null {
  const trimmed = raw.trim();
  if (!trimmed || trimmed.includes("/") || !trimmed.includes("@")) return null;
  return trimmed as MucRoomJid;
}

export function jidDomain(jid: string): string {
  const bareJid = barePeerJid(jid);
  return bareJid.split("@")[1] ?? "localhost";
}

export function barePeerJid(fullJid: string): string {
  return fullJid.split("/")[0] ?? "";
}

/**
 * Localpart of a JID (text before the first `@` of the bare JID).
 * Mirrors `bare.split("@")[0]`: a bare JID without an `@` is returned as-is.
 */
export function jidLocalpart(jid: string): string {
  return barePeerJid(jid).split("@")[0] ?? "";
}

/** Domain part of a JID, or `""` when the bare JID contains no `@`. */
export function jidDomainOrEmpty(jid: string): string {
  return barePeerJid(jid).split("@")[1] ?? "";
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

/**
 * Comparison key for bare-JID equality across sources: server-emitted
 * JIDs are RFC 7622-lowercased while directory/UI-derived JIDs may
 * carry the original case, so byte comparison silently misses.
 */
export function bareJidKey(jid: string): string {
  return barePeerJid(jid).trim().toLowerCase();
}

/**
 * Conversation identity for direct-message coordination. The caller supplies
 * session-authoritative MUC service context; syntax alone cannot distinguish
 * an occupant from an ordinary account resource.
 */
export function dmConversationIdentityKey(
  jid: string,
  isMucPmPeer: (peerJid: string) => boolean,
): string {
  return isMucPmPeer(jid) ? fullJidIdentityKey(jid) : bareJidKey(jid);
}

export function parseManagedRoomBareJid(
  roomJid: string,
): { channelId: string } | null {
  const node = jidLocalpart(roomJid);
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

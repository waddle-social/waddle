import { bareJidKey, jidDomainOrEmpty } from "@/lib/xmpp/jid";

// XEP-0422 §Business Rules leaves "who may fasten this" to the payload
// spec. Safety scores are server judgments, so only the server side of
// the conversation may attach them — never an occupant or a DM peer, who
// could otherwise pin fabricated scores onto someone else's message.

/** MUC: only the room itself (its bare JID, no occupant resource). */
export function isTrustedRoomSafetyScoresSender(fromJid: string, roomJid: string): boolean {
  return !!roomJid && !fromJid.includes("/") && bareJidKey(fromJid) === bareJidKey(roomJid);
}

/** 1:1 chat: only the account's own server (its domain JID). */
export function isTrustedDmSafetyScoresSender(fromJid: string, selfBareJid: string): boolean {
  const domain = jidDomainOrEmpty(selfBareJid).toLowerCase();
  return !!domain && fromJid.trim().toLowerCase() === domain;
}

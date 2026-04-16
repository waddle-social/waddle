import type { Agent } from "stanza";
import type { ChatStateType } from "./types";

export function sendDmChatState(xmpp: Agent, peerJid: string, state: ChatStateType): void {
  xmpp.sendMessage({ to: peerJid, type: "chat", chatState: state, processingHints: { noStore: true } });
}

export function sendDmDisplayed(xmpp: Agent, peerJid: string, messageId: string): void {
  xmpp.sendMessage({
    to: peerJid,
    type: "chat",
    marker: { type: "displayed", id: messageId },
    processingHints: { noStore: true },
  });
}

export function sendDmReaction(xmpp: Agent, peerJid: string, messageId: string, emojis: string[]): void {
  xmpp.sendMessage({
    id: crypto.randomUUID(),
    to: peerJid,
    type: "chat",
    reactions: { id: messageId, items: emojis },
    processingHints: { store: true },
  } as Record<string, unknown>);
}

export function sendDmRetraction(xmpp: Agent, peerJid: string, retractsId: string): void {
  xmpp.sendMessage({
    id: crypto.randomUUID(),
    to: peerJid,
    type: "chat",
    body: "This person attempted to retract a previous message.",
    retract: { id: retractsId },
    processingHints: { store: true },
  } as Record<string, unknown>);
}

export function sendDmCorrection(xmpp: Agent, peerJid: string, body: string, replacesId: string): string | null {
  const text = body.trim();
  if (!text) return null;
  const msgId = crypto.randomUUID();
  xmpp.sendMessage({ id: msgId, to: peerJid, type: "chat", body: text, replace: replacesId, processingHints: { store: true } });
  return msgId;
}

export function sendDirectMessage(xmpp: Agent, peerJid: string, body: string): string | null {
  const text = body.trim();
  if (!text) return null;
  const msgId = crypto.randomUUID();
  xmpp.sendMessage({
    id: msgId,
    to: peerJid,
    type: "chat",
    body: text,
    receipt: { type: "request" },
    marker: { type: "markable" },
    processingHints: { store: true },
  } as Record<string, unknown>);
  return msgId;
}

export function sendDmCallInvite(
  xmpp: Agent,
  peerJid: string,
  opts: { inviteId?: string; sid?: string; jingleJid?: string; externalUri?: string; video: boolean; muji?: boolean },
): string {
  const msgId = opts.inviteId ?? crypto.randomUUID();
  const label = opts.video ? "Video call" : "Audio call";
  const externalUri = opts.externalUri ?? (opts.jingleJid ? `xmpp:${opts.jingleJid}` : undefined);
  const callInvite: Record<string, unknown> = {
    id: msgId,
    muji: opts.muji ?? false,
  };
  if (opts.sid) callInvite.jingleSid = opts.sid;
  if (opts.jingleJid) callInvite.jingleJid = opts.jingleJid;
  if (externalUri) callInvite.externalUri = externalUri;
  callInvite.video = !!opts.video;
  xmpp.sendMessage({
    id: msgId,
    to: peerJid,
    type: "chat",
    body: opts.sid ? `${label} started` : (externalUri ? `${label}: ${externalUri}` : label),
    callInvite,
    meeting: { type: "dm", ...(externalUri ? { url: externalUri } : {}), desc: label },
    processingHints: { store: true },
  } as Record<string, unknown>);
  return msgId;
}

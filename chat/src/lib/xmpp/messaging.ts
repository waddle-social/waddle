/** Outbound message operations — all send* functions as standalone. */
import type { Agent } from "stanza";
import type { MarkupSpan } from "@/lib/chat-ui";
import type { WaddleMarkupSpan } from "./extensions/markup";
import type { ChatStateType } from "./types";

export function sendChatState(xmpp: Agent, roomJid: string, state: ChatStateType): void {
  xmpp.sendMessage({ to: roomJid, type: "groupchat", chatState: state });
}

export function sendDisplayed(xmpp: Agent, roomJid: string, messageId: string): void {
  xmpp.sendMessage({
    to: roomJid,
    type: "groupchat",
    marker: { type: "displayed", id: messageId },
  });
}

export function sendReaction(xmpp: Agent, roomJid: string, messageId: string, emojis: string[]): void {
  xmpp.sendMessage({
    id: crypto.randomUUID(),
    to: roomJid,
    type: "groupchat",
    reactions: { id: messageId, items: emojis },
  } as Record<string, unknown>);
}

export function sendRetraction(xmpp: Agent, roomJid: string, retractsId: string): void {
  xmpp.sendMessage({
    id: crypto.randomUUID(),
    to: roomJid,
    type: "groupchat",
    body: "This person attempted to retract a previous message.",
    retract: { id: retractsId },
  } as Record<string, unknown>);
}

export function sendModeration(xmpp: Agent, roomJid: string, targetId: string, reason?: string): void {
  xmpp.sendMessage({
    id: crypto.randomUUID(),
    to: roomJid,
    type: "groupchat",
    applyTo: {
      id: targetId,
      moderated: { retract: true, ...(reason ? { reason } : {}) },
    },
  } as Record<string, unknown>);
}

function toStanzaSpans(spans: MarkupSpan[]): WaddleMarkupSpan[] {
  return spans.map(s => ({ type: s.type, start: s.start, end: s.end, ...(s.uri ? { uri: s.uri } : {}) }));
}

export function sendCorrection(xmpp: Agent, roomJid: string, body: string, replacesId: string, markup?: MarkupSpan[]): string | null {
  const text = body.trim();
  if (!text) return null;
  const msgId = crypto.randomUUID();
  const msgData: Record<string, unknown> = { id: msgId, to: roomJid, type: "groupchat", body: text, replace: replacesId };
  if (markup && markup.length > 0) {
    msgData.markup = { spans: toStanzaSpans(markup) };
  }
  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

export function sendGroupMessage(xmpp: Agent, roomJid: string, body: string, markup?: MarkupSpan[]): string | null {
  const text = body.trim();
  if (!text) return null;

  const msgId = crypto.randomUUID();

  // XEP-0372: Build reference objects for @mentions
  const references: Array<{ type: string; uri: string; begin: string; end: string }> = [];
  const mentionRe = /(?:^|\s)@(\S+)/g;
  let match: RegExpExecArray | null;
  while ((match = mentionRe.exec(text)) !== null) {
    const nick = match[1]!;
    const begin = match.index + (match[0].length - nick.length - 1);
    const end = begin + nick.length + 1;
    references.push({ type: "mention", begin: String(begin), end: String(end), uri: `xmpp:${nick}` });
  }

  // XEP-0513: Explicit @everyone / @here
  const explicitMentionItems: Array<{ type: string }> = [];
  if (/(?:^|\s)@everyone(?:\s|$)/i.test(text)) explicitMentionItems.push({ type: "everyone" });
  if (/(?:^|\s)@here(?:\s|$)/i.test(text)) explicitMentionItems.push({ type: "here" });

  const msgData: Record<string, unknown> = {
    id: msgId, to: roomJid, type: "groupchat", body: text,
    receipt: { type: "request" },
    marker: { type: "markable" },
  };
  if (references.length > 0) msgData.references = references;
  if (explicitMentionItems.length > 0) msgData.explicitMentions = { items: explicitMentionItems };
  if (markup && markup.length > 0) {
    msgData.markup = { spans: toStanzaSpans(markup) };
  }

  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

export interface SendCallInviteOptions {
  inviteId?: string;
  sid?: string;
  jingleJid?: string;
  externalUri?: string;
  video: boolean;
  muji?: boolean;
}

export function sendCallInvite(xmpp: Agent, roomJid: string, opts: SendCallInviteOptions): string {
  const msgId = opts.inviteId ?? crypto.randomUUID();
  const label = opts.video ? "Video call" : "Audio call";
  const externalUri = opts.externalUri ?? (opts.jingleJid ? `xmpp:${opts.jingleJid}` : undefined);
  const callInvite: Record<string, unknown> = {
    id: msgId,
    muji: opts.muji ?? true,
  };
  if (opts.sid) callInvite.jingleSid = opts.sid;
  if (opts.jingleJid) callInvite.jingleJid = opts.jingleJid;
  if (externalUri) callInvite.externalUri = externalUri;

  xmpp.sendMessage({
    id: msgId, to: roomJid, type: "groupchat",
    body: opts.sid ? `${label} started` : (externalUri ? `${label}: ${externalUri}` : label),
    callInvite,
    meeting: { type: "muji", ...(externalUri ? { url: externalUri } : {}), desc: label },
  } as Record<string, unknown>);

  return msgId;
}

export function sendCallReject(xmpp: Agent, roomJid: string, inviteId: string): void {
  xmpp.sendMessage({
    id: crypto.randomUUID(),
    to: roomJid,
    type: "groupchat",
    callReject: { id: inviteId },
  } as Record<string, unknown>);
}

export function sendCallLeft(xmpp: Agent, roomJid: string, inviteId?: string): void {
  xmpp.sendMessage({
    id: crypto.randomUUID(),
    to: roomJid,
    type: "groupchat",
    callLeft: inviteId ? { id: inviteId } : {},
  } as Record<string, unknown>);
}

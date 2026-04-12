/** Outbound message operations — all send* functions as standalone. */
import type { Agent } from "stanza";
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

export function sendCorrection(xmpp: Agent, roomJid: string, body: string, replacesId: string): string | null {
  const text = body.trim();
  if (!text) return null;
  const msgId = crypto.randomUUID();
  xmpp.sendMessage({ id: msgId, to: roomJid, type: "groupchat", body: text, replace: replacesId });
  return msgId;
}

export function sendGroupMessage(xmpp: Agent, roomJid: string, body: string): string | null {
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

  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

export function sendCallInvite(xmpp: Agent, roomJid: string, meetingUrl: string, video: boolean): string {
  const msgId = crypto.randomUUID();
  const sessionId = crypto.randomUUID();
  const label = video ? "Video call" : "Audio call";

  xmpp.sendMessage({
    id: msgId, to: roomJid, type: "groupchat",
    body: `${label}: ${meetingUrl}`,
    callPropose: { id: sessionId, audio: true, video, externalUri: meetingUrl },
    meeting: { type: "jitsi", url: meetingUrl, desc: label },
  } as Record<string, unknown>);

  return msgId;
}

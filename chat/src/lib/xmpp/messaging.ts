/** Outbound message operations — all send* functions as standalone. */
import type { Agent } from "stanza";
import type { MarkupSpan } from "@/lib/chat-ui";
import type { WaddleMarkupSpan } from "./extensions/markup";
import type { ChatStateType } from "./types";

export interface OutboundFileAttachment {
  url: string;
  name: string;
  mediaType: string;
  size: number;
  width?: number;
  height?: number;
}

/** Build one XEP-0447 <file-sharing> payload for a single attachment. */
export function fileSharingElement(file: OutboundFileAttachment): Record<string, unknown> {
  return {
    disposition: "inline",
    name: file.name,
    mediaType: file.mediaType,
    size: String(file.size),
    ...(file.width ? { width: String(file.width) } : {}),
    ...(file.height ? { height: String(file.height) } : {}),
    url: file.url,
  };
}

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
    processingHints: { store: true },
  } as Record<string, unknown>);
}

export function sendRetraction(xmpp: Agent, roomJid: string, retractsId: string): void {
  xmpp.sendMessage({
    id: crypto.randomUUID(),
    to: roomJid,
    type: "groupchat",
    body: "This person attempted to retract a previous message.",
    retract: { id: retractsId },
    processingHints: { store: true },
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
    processingHints: { store: true },
  } as Record<string, unknown>);
}

function toStanzaSpans(spans: MarkupSpan[]): WaddleMarkupSpan[] {
  return spans.map(s => ({ type: s.type, start: s.start, end: s.end, ...(s.uri ? { uri: s.uri } : {}) }));
}

export function sendCorrection(xmpp: Agent, roomJid: string, body: string, replacesId: string, markup?: MarkupSpan[]): string | null {
  const text = body.trim();
  if (!text) return null;
  const msgId = crypto.randomUUID();
  const msgData: Record<string, unknown> = {
    id: msgId,
    to: roomJid,
    type: "groupchat",
    body: text,
    replace: replacesId,
    processingHints: { store: true },
  };
  if (markup && markup.length > 0) {
    msgData.markup = { spans: toStanzaSpans(markup) };
  }
  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

/**
 * Send a groupchat message. Optionally includes XEP-0447 file-sharing
 * attachments alongside the user's text body in a single stanza.
 */
export function sendGroupMessage(
  xmpp: Agent,
  roomJid: string,
  body: string,
  markup?: MarkupSpan[],
  files?: OutboundFileAttachment[],
): string | null {
  const text = body.trim();
  const hasFiles = !!files && files.length > 0;
  if (!text && !hasFiles) return null;

  const msgId = crypto.randomUUID();
  const effectiveBody = text || (hasFiles ? files![0].url : "");

  // XEP-0372: Build reference objects for @mentions (only scan user text)
  const references: Array<{ type: string; uri: string; begin: string; end: string }> = [];
  if (text) {
    const mentionRe = /(?:^|\s)@(\S+)/g;
    let match: RegExpExecArray | null;
    while ((match = mentionRe.exec(text)) !== null) {
      const nick = match[1]!;
      const begin = match.index + (match[0].length - nick.length - 1);
      const end = begin + nick.length + 1;
      references.push({ type: "mention", begin: String(begin), end: String(end), uri: `xmpp:${nick}` });
    }
  }

  // XEP-0513: Explicit @everyone / @here
  const explicitMentionItems: Array<{ type: string }> = [];
  if (text && /(?:^|\s)@everyone(?:\s|$)/i.test(text)) explicitMentionItems.push({ type: "everyone" });
  if (text && /(?:^|\s)@here(?:\s|$)/i.test(text)) explicitMentionItems.push({ type: "here" });

  const msgData: Record<string, unknown> = {
    id: msgId, to: roomJid, type: "groupchat", body: effectiveBody,
    receipt: { type: "request" },
    marker: { type: "markable" },
    processingHints: { store: true },
  };
  if (references.length > 0) msgData.references = references;
  if (explicitMentionItems.length > 0) msgData.explicitMentions = { items: explicitMentionItems };
  if (markup && markup.length > 0) {
    msgData.markup = { spans: toStanzaSpans(markup) };
  }
  if (hasFiles) {
    msgData.fileSharing = files!.map(fileSharingElement);
    msgData.links = files!.map((f) => ({ url: f.url }));
    if (!text) {
      // Body is acting as the SFS URL fallback — mark it so compliant clients skip it.
      msgData.fallback = { for: "urn:xmpp:sfs:0", body: true };
    }
  }

  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

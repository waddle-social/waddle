import type { Agent } from "stanza";
import type { ChatStateType } from "./types";
import { fileSharingElement, type OutboundFileAttachment } from "./messaging";

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

/**
 * Send a direct (type="chat") message. Optionally includes XEP-0447 file-sharing
 * attachments alongside the user's text body in a single stanza.
 */
export function sendDirectMessage(
  xmpp: Agent,
  peerJid: string,
  body: string,
  files?: OutboundFileAttachment[],
): string | null {
  const text = body.trim();
  const hasFiles = !!files && files.length > 0;
  if (!text && !hasFiles) return null;

  const msgId = crypto.randomUUID();
  const effectiveBody = text || (hasFiles ? files![0].url : "");

  const msgData: Record<string, unknown> = {
    id: msgId,
    to: peerJid,
    type: "chat",
    body: effectiveBody,
    receipt: { type: "request" },
    marker: { type: "markable" },
    processingHints: { store: true },
  };
  if (hasFiles) {
    msgData.fileSharing = files!.map(fileSharingElement);
    msgData.links = files!.map((f) => ({ url: f.url }));
    if (!text) {
      msgData.fallback = { for: "urn:xmpp:sfs:0", body: true };
    }
  }

  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

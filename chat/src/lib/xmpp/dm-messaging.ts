import type { Agent } from "stanza";
import type { ChatStateType } from "./types";
import {
  buildReplyFallbackPrefix,
  fileSharingElement,
  type OutboundFileAttachment,
  type ReplyTarget,
} from "./messaging";
import type { WaddleFallback } from "./extensions/fallback";

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

export interface SendDirectMessageOptions {
  files?: OutboundFileAttachment[];
  replyTo?: ReplyTarget;
  threadId?: string;
  parentThreadId?: string;
  id?: string;
}

/**
 * Send a direct (type="chat") message. Supports XEP-0447 attachments, XEP-0461
 * replies with XEP-0428 fallback prefix, and RFC 6121 / XEP-0201 threads.
 *
 * Every message carries a `<thread>` child: replies inherit their parent's
 * thread id, originals self-identify with `threadId === msgId` so the root
 * can be discovered by MAM-by-thread and thread-aware UI.
 *
 * Pass a pre-generated `id` when the caller already created an optimistic
 * timeline entry so the stanza ID matches.
 */
export function sendDirectMessage(
  xmpp: Agent,
  peerJid: string,
  body: string,
  opts: SendDirectMessageOptions = {},
): string | null {
  const { files, replyTo, threadId, parentThreadId, id } = opts;
  const text = body.trim();
  const hasFiles = !!files && files.length > 0;
  if (!text && !hasFiles) return null;

  const msgId = id ?? crypto.randomUUID();
  const { prefix, length: prefixLength } = replyTo
    ? buildReplyFallbackPrefix(replyTo.body)
    : { prefix: "", length: 0 };
  const bodyText = text || (hasFiles ? files![0].url : "");
  const effectiveBody = prefix + bodyText;
  const fallbacks: WaddleFallback[] = [];
  if (replyTo && prefixLength > 0) {
    fallbacks.push({ for: "urn:xmpp:reply:0", body: { start: 0, end: prefixLength } });
  }

  const msgData: Record<string, unknown> = {
    id: msgId,
    to: peerJid,
    type: "chat",
    body: effectiveBody,
    receipt: { type: "request" },
    marker: { type: "markable" },
    processingHints: { store: true },
  };
  if (replyTo) {
    msgData.reply = { to: replyTo.author, id: replyTo.id };
  }
  // XEP-0201: always emit `<thread>`; originals use their own msgId.
  msgData.thread = threadId ?? msgId;
  if (parentThreadId) msgData.parentThread = parentThreadId;
  if (hasFiles) {
    msgData.fileSharing = files!.map(fileSharingElement);
    msgData.links = files!.map((f) => ({ url: f.url }));
    if (!text) {
      fallbacks.push({ for: "urn:xmpp:sfs:0" });
    }
  }
  if (fallbacks.length > 0) msgData.fallbacks = fallbacks;

  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

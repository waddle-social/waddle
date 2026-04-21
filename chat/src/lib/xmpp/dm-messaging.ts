import type { Agent } from "stanza";
import type { ChatStateType } from "./types";
import {
  buildReplyFallbackPrefix,
  fileSharingElement,
  type OutboundFileAttachment,
  type ReplyTarget,
} from "./messaging";
import type { WaddleFallback } from "./extensions/fallback";
import type { WaddleEncryptedFile } from "./extensions/encrypted-file";
import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";
import { shiftMarkupSpans } from "./extensions/markup";
import { codePointLength } from "@/lib/text-offsets";

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

function shiftReferences(references: readonly MessageReference[] | undefined, offset: number): MessageReference[] | undefined {
  if (!references?.length) return undefined;
  return references.map((reference) => {
    if (typeof reference.begin !== "number" || typeof reference.end !== "number") return reference;
    return { ...reference, begin: reference.begin + offset, end: reference.end + offset };
  });
}

function toStanzaReferences(references: readonly MessageReference[]): Array<Record<string, string>> {
  return references.map((reference) => ({
    type: reference.type,
    uri: reference.uri,
    ...(typeof reference.begin === "number" ? { begin: String(reference.begin) } : {}),
    ...(typeof reference.end === "number" ? { end: String(reference.end) } : {}),
    ...(reference.anchor ? { anchor: reference.anchor } : {}),
  }));
}

export function sendDmCorrection(
  xmpp: Agent,
  peerJid: string,
  body: string,
  replacesId: string,
  markup?: MarkupSpan[],
  references?: MessageReference[],
): string | null {
  if (!body.trim()) return null;
  const msgId = crypto.randomUUID();
  const msgData: Record<string, unknown> = {
    id: msgId,
    to: peerJid,
    type: "chat",
    body,
    replace: replacesId,
    processingHints: { store: true },
  };
  if (markup?.length) msgData.markup = { spans: markup };
  if (references?.length) msgData.references = toStanzaReferences(references);
  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

export interface SendDirectMessageOptions {
  markup?: MarkupSpan[];
  references?: MessageReference[];
  files?: OutboundFileAttachment[];
  replyTo?: ReplyTarget;
  threadId?: string;
  parentThreadId?: string;
  id?: string;
}

/**
 * Send a direct (type="chat") message. Supports XEP-0447/XEP-0448 attachments,
 * XEP-0461 replies with XEP-0428 fallback prefix, and RFC 6121 / XEP-0201
 * threads.
 *
 * `<reply>` and `<thread>` are independent: bare replies stay inline in the
 * conversation; `<thread>` is only attached when the caller explicitly wants
 * thread membership.
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
  const { markup, references, files, replyTo, threadId, parentThreadId, id } = opts;
  const text = body;
  const hasText = text.trim().length > 0;
  const hasFiles = !!files && files.length > 0;
  if (!hasText && !hasFiles) return null;

  const msgId = id ?? crypto.randomUUID();
  const { prefix, length: prefixLength } = replyTo
    ? buildReplyFallbackPrefix(replyTo.body)
    : { prefix: "", length: 0 };
  const markupPrefixLength = codePointLength(prefix);
  const bodyText = hasText ? text : (hasFiles ? files![0].url : "");
  const effectiveBody = prefix + bodyText;
  const rebasedMarkup = markup?.length ? shiftMarkupSpans(markup, markupPrefixLength) : undefined;
  const rebasedReferences = shiftReferences(references, markupPrefixLength);
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
  if (rebasedMarkup?.length) msgData.markup = { spans: rebasedMarkup };
  if (rebasedReferences?.length) msgData.references = toStanzaReferences(rebasedReferences);
  if (replyTo) {
    msgData.reply = { to: replyTo.author, id: replyTo.id };
  }
  if (threadId) {
    msgData.thread = threadId;
    if (parentThreadId) msgData.parentThread = parentThreadId;
  }
  if (hasFiles) {
    msgData.fileSharing = files!.map(fileSharingElement);
    msgData.links = files!.map((f) => ({ url: f.url }));
    const encryptedFiles: WaddleEncryptedFile[] = files!
      .map((file): WaddleEncryptedFile | null => {
        if (!file.encrypted) return null;
        const sources = file.encrypted.sources?.filter((value) => value.length > 0) ?? [];
        return {
          ...file.encrypted,
          sources: sources.length > 0 ? sources : [file.url],
        };
      })
      .filter((value): value is WaddleEncryptedFile => value !== null);
    if (encryptedFiles.length > 0) {
      msgData.encryptedFiles = encryptedFiles;
    }
    if (!hasText) {
      fallbacks.push({ for: "urn:xmpp:sfs:0" });
    }
  }
  if (fallbacks.length > 0) msgData.fallbacks = fallbacks;

  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

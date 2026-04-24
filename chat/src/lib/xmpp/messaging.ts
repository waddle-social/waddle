/** Outbound message operations — all send* functions as standalone. */
import type { Agent } from "stanza";
import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { WaddleFallback } from "./extensions/fallback";
import type { WaddleThreadCreate, WaddleThreadReply } from "./extensions/forums";
import type { WaddleEncryptedFile } from "./extensions/encrypted-file";
import { shiftMarkupSpans, type WaddleMarkupSpan } from "./extensions/markup";
import type { ChatStateType } from "./types";
import { isBroadcastMention, resolveMentionUri } from "@/lib/mentions";
import { codePointLength } from "@/lib/text-offsets";

export interface OutboundFileAttachment {
  url: string;
  name: string;
  mediaType: string;
  size: number;
  disposition: "inline" | "attachment";
  width?: number;
  height?: number;
  encrypted?: WaddleEncryptedFile;
}

export interface ReplyTarget {
  /** Stanza id of the message being replied to. */
  id: string;
  /** JID (user or room occupant) of the original author. */
  author: string;
  /** Original message body, used to build the > quoted fallback prefix. */
  body?: string;
}

/** Build one XEP-0447 <file-sharing> payload for a single attachment. */
export function fileSharingElement(file: OutboundFileAttachment): Record<string, unknown> {
  return {
    disposition: file.disposition,
    name: file.name,
    mediaType: file.mediaType,
    size: String(file.size),
    ...(file.width ? { width: String(file.width) } : {}),
    ...(file.height ? { height: String(file.height) } : {}),
    url: file.url,
  };
}

function encryptedFileElement(file: OutboundFileAttachment): WaddleEncryptedFile | null {
  if (!file.encrypted) return null;
  const sources = file.encrypted.sources?.filter((value) => value.length > 0) ?? [];
  return {
    ...file.encrypted,
    sources: sources.length > 0 ? sources : [file.url],
  };
}

/**
 * Build a `> quoted\n\n` fallback prefix for a reply.
 *
 * Returns the prefix string and its string-length range so callers can attach
 * an XEP-0428 `<fallback/>` body range alongside the outbound text.
 */
export function buildReplyFallbackPrefix(parentBody: string | undefined): { prefix: string; length: number } {
  if (!parentBody) return { prefix: "", length: 0 };
  const lines = parentBody.split("\n");
  const quoted = lines.map((line) => `> ${line}`).join("\n");
  const prefix = `${quoted}\n\n`;
  return { prefix, length: codePointLength(prefix) };
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
  return spans;
}

function shiftReferences(references: readonly MessageReference[] | undefined, offset: number): MessageReference[] | undefined {
  if (!references?.length) return undefined;
  const shifted = references.flatMap((reference) => {
    if (typeof reference.begin !== "number" || typeof reference.end !== "number") return [reference];
    return [{ ...reference, begin: reference.begin + offset, end: reference.end + offset }];
  });
  return shifted.length > 0 ? shifted : undefined;
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

interface MentionToken {
  token: string;
  begin: number;
  end: number;
}

const INLINE_MENTION_RE = /(^|[\s])@([^\s<.,;:!?'")\]}&]+?)(?=[\s<.,;:!?'")\]}&]|$)/g;

function extractMentionTokens(text: string, offset: number): MentionToken[] {
  const tokens: MentionToken[] = [];
  let match: RegExpExecArray | null;

  while ((match = INLINE_MENTION_RE.exec(text)) !== null) {
    const leading = match[1] ?? "";
    const token = match[2] ?? "";
    if (!token) continue;

    const mentionStartCodeUnits = match.index + leading.length;
    const begin = offset + codePointLength(text.slice(0, mentionStartCodeUnits));
    const end = begin + codePointLength(`@${token}`);
    tokens.push({ token, begin, end });
  }

  return tokens;
}

export function sendCorrection(
  xmpp: Agent,
  roomJid: string,
  body: string,
  replacesId: string,
  markup?: MarkupSpan[],
  references?: MessageReference[],
): string | null {
  if (!body.trim()) return null;
  const msgId = crypto.randomUUID();
  const mentionTokens = extractMentionTokens(body, 0);
  const stanzaReferences: MessageReference[] = references ? [...references] : [];
  for (const mention of mentionTokens) {
    if (isBroadcastMention(mention.token)) continue;
    stanzaReferences.push({
      type: "mention",
      begin: mention.begin,
      end: mention.end,
      uri: resolveMentionUri(mention.token),
    });
  }
  const explicitMentions: Array<{ mentions: string; active?: boolean }> = [];
  if (mentionTokens.some((mention) => mention.token.toLowerCase() === "everyone")) {
    explicitMentions.push({ mentions: "urn:xmpp:mentions:0#channel" });
  }
  if (mentionTokens.some((mention) => mention.token.toLowerCase() === "here")) {
    explicitMentions.push({ mentions: "urn:xmpp:mentions:0#channel", active: true });
  }
  const msgData: Record<string, unknown> = {
    id: msgId,
    to: roomJid,
    type: "groupchat",
    body,
    replace: replacesId,
    processingHints: { store: true },
  };
  if (markup && markup.length > 0) {
    msgData.markup = { spans: toStanzaSpans(markup) };
  }
  if (stanzaReferences.length > 0) {
    msgData.references = toStanzaReferences(stanzaReferences);
  }
  if (explicitMentions.length > 0) msgData.explicitMentions = explicitMentions;
  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

export interface SendGroupMessageOptions {
  markup?: MarkupSpan[];
  references?: MessageReference[];
  mentionJidsByNick?: Readonly<Record<string, string>>;
  files?: OutboundFileAttachment[];
  replyTo?: ReplyTarget;
  threadId?: string;
  parentThreadId?: string;
  id?: string;
  threadCreate?: WaddleThreadCreate;
  threadReply?: WaddleThreadReply;
}

/**
 * Send a groupchat message. Supports XEP-0447/XEP-0448 attachments, XEP-0461
 * replies with XEP-0428 fallback prefix, and RFC 6121 / XEP-0201 threads.
 *
 * `<reply>` and `<thread>` are independent: a bare `<reply>` is a quote that
 * stays inline in the feed; a `<thread>` tags membership in a conversation
 * group and moves the message into the thread panel. The caller passes
 * `threadId` only when it explicitly wants the message threaded.
 *
 * Pass a pre-generated `id` when the caller already created an optimistic
 * timeline entry so the stanza ID matches.
 */
export function sendGroupMessage(
  xmpp: Agent,
  roomJid: string,
  body: string,
  opts: SendGroupMessageOptions = {},
): string | null {
  const {
    markup,
    references: richReferences,
    mentionJidsByNick,
    files,
    replyTo,
    threadId,
    parentThreadId,
    id,
    threadCreate,
    threadReply,
  } = opts;
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
  const rebasedMarkup = markup && markup.length > 0
    ? shiftMarkupSpans(markup, markupPrefixLength)
    : undefined;
  const rebasedRichReferences = shiftReferences(richReferences, markupPrefixLength);

  // XEP-0372: Build reference objects for @mentions (only scan user text)
  const references: MessageReference[] = rebasedRichReferences ? [...rebasedRichReferences] : [];
  const mentionTokens = hasText ? extractMentionTokens(text, prefixLength) : [];
  for (const mention of mentionTokens) {
    if (isBroadcastMention(mention.token)) continue;
    references.push({
      type: "mention",
      begin: mention.begin,
      end: mention.end,
      uri: resolveMentionUri(mention.token, mentionJidsByNick),
    });
  }

  // XEP-0513: Explicit channel mentions. @here is represented as a channel
  // mention with the XEP-0513 active modifier.
  const explicitMentions: Array<{ mentions: string; active?: boolean }> = [];
  if (mentionTokens.some((mention) => mention.token.toLowerCase() === "everyone")) {
    explicitMentions.push({ mentions: "urn:xmpp:mentions:0#channel" });
  }
  if (mentionTokens.some((mention) => mention.token.toLowerCase() === "here")) {
    explicitMentions.push({ mentions: "urn:xmpp:mentions:0#channel", active: true });
  }

  const fallbacks: WaddleFallback[] = [];
  if (replyTo && prefixLength > 0) {
    fallbacks.push({ for: "urn:xmpp:reply:0", body: { start: 0, end: prefixLength } });
  }

  const msgData: Record<string, unknown> = {
    id: msgId, to: roomJid, type: "groupchat", body: effectiveBody,
    receipt: { type: "request" },
    marker: { type: "markable" },
    processingHints: { store: true },
  };
  if (references.length > 0) msgData.references = toStanzaReferences(references);
  if (explicitMentions.length > 0) msgData.explicitMentions = explicitMentions;
  if (rebasedMarkup && rebasedMarkup.length > 0) {
    msgData.markup = { spans: toStanzaSpans(rebasedMarkup) };
  }
  if (replyTo) {
    msgData.reply = { to: replyTo.author, id: replyTo.id };
  }
  if (threadId) {
    msgData.thread = threadId;
    if (parentThreadId) msgData.parentThread = parentThreadId;
  }
  if (threadCreate) {
    msgData.threadCreate = threadCreate;
  }
  if (threadReply) {
    msgData.threadReply = threadReply;
  }
  if (hasFiles) {
    msgData.fileSharing = files!.map(fileSharingElement);
    msgData.links = files!.map((f) => ({ url: f.url }));
    const encryptedFiles = files!
      .map(encryptedFileElement)
      .filter((value): value is WaddleEncryptedFile => value !== null);
    if (encryptedFiles.length > 0) {
      msgData.encryptedFiles = encryptedFiles;
    }
    if (!hasText) {
      // Body is acting as the SFS URL fallback — mark it so compliant clients skip it.
      fallbacks.push({ for: "urn:xmpp:sfs:0" });
    }
  }
  if (fallbacks.length > 0) msgData.fallbacks = fallbacks;

  xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);
  return msgId;
}

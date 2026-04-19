/** Inbound message parsing — extracts XEP extension data from stanza messages. */
import type { ReceivedMessage } from "stanza/protocol";
import type { WaddleEncryptedFile } from "./extensions/encrypted-file";
import { stripMarkupRange } from "./extensions/markup";
import type {
  ChatStateEvent, ChatStateType, DisplayedEvent,
  LiveDmMessage, LiveRoomMessage, ReactionEvent, RoomActivityEvent, SharedFileInfo,
} from "./types";

type MessageExtensionsTarget = Pick<
  LiveRoomMessage,
  | "body"
  | "mentions"
  | "broadcastMention"
  | "sharedFiles"
  | "isSticker"
  | "replacesId"
  | "markup"
  | "replyTo"
  | "threadId"
  | "parentThreadId"
  | "forumPostKind"
  | "forumTitle"
  | "forumThreadTitle"
>;

/** Access custom JXT extension fields that TypeScript doesn't know about. */
export function ext(msg: unknown): Record<string, unknown> {
  return msg as Record<string, unknown>;
}

const encoder = new TextEncoder();

function byteLen(text: string): number {
  return encoder.encode(text).byteLength;
}

/** Populate a LiveRoomMessage with data from XEP extensions on the stanza. */
export function extractMessageExtensions(
  msg: ReceivedMessage,
  base: LiveRoomMessage | LiveDmMessage,
): void {
  if (msg.replace) {
    base.replacesId = msg.replace;
  }

  extractReferences(msg, base);
  extractExplicitMentions(msg, base);
  extractFileSharing(msg, base);
  extractMarkup(msg, base);
  extractReplyAndThread(msg, base);
  stripReplyFallback(msg, base);

  if (ext(msg).sticker) {
    base.isSticker = true;
  }
}

/** XEP-0461 reply pointer + RFC 6121 / XEP-0201 thread id + parent. */
function extractReplyAndThread(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const reply = ext(msg).reply as { to?: string; id?: string } | undefined;
  if (reply?.id) {
    base.replyTo = { id: reply.id, ...(reply.to ? { author: reply.to } : {}) };
  }
  const threadId = ext(msg).thread as string | undefined;
  if (threadId) base.threadId = threadId;
  const parentThread = ext(msg).parentThread as string | undefined;
  if (parentThread) base.parentThreadId = parentThread;
  const threadCreate = ext(msg).threadCreate as { title?: string } | undefined;
  if (threadCreate?.title?.trim()) {
    base.forumPostKind = "topic";
    base.forumTitle = threadCreate.title.trim();
    base.forumThreadTitle = threadCreate.title.trim();
    if (!base.threadId && msg.id) base.threadId = msg.id;
  }
  const threadReply = ext(msg).threadReply as { threadId?: string } | undefined;
  if (threadReply?.threadId) {
    base.forumPostKind = "reply";
    if (!base.threadId) base.threadId = threadReply.threadId;
  }
}

interface FallbackPayload {
  for?: string;
  body?: { start?: number; end?: number };
}

/**
 * XEP-0428: strip any `urn:xmpp:reply:0` fallback range from the displayed
 * body so the `> quoted` prefix doesn't double-render on top of the reply chip.
 */
function stripReplyFallback(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const fallbacks = ext(msg).fallbacks as FallbackPayload[] | undefined;
  if (!fallbacks?.length || !base.body) return;
  const range = fallbacks.find((f) => f.for === "urn:xmpp:reply:0")?.body;
  if (!range) return;
  const rawStart = range.start ?? 0;
  const rawEnd = range.end ?? rawStart;
  if (!Number.isFinite(rawStart) || !Number.isFinite(rawEnd) || rawStart < 0 || rawEnd < 0) {
    return;
  }
  const start = Math.max(0, Math.min(rawStart, base.body.length));
  const end = Math.max(start, Math.min(rawEnd, base.body.length));
  if (end <= start) return;
  const prefixText = base.body.slice(0, start);
  const strippedText = base.body.slice(start, end);
  const markupRangeStart = byteLen(prefixText);
  const markupRangeEnd = markupRangeStart + byteLen(strippedText);
  base.body = base.body.slice(0, start) + base.body.slice(end);
  if (!base.markup?.length) return;
  const rebasedMarkup = stripMarkupRange(base.markup, markupRangeStart, markupRangeEnd);
  if (rebasedMarkup.length > 0) {
    base.markup = rebasedMarkup;
    return;
  }
  delete base.markup;
}

function extractReferences(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const refs = ext(msg).references as Array<{ type?: string; uri?: string }> | undefined;
  if (!refs?.length) return;

  const mentionUris = refs
    .filter((r) => r.type === "mention" && r.uri)
    .map((r) => (r.uri as string).replace(/^xmpp:/, ""));
  if (mentionUris.length > 0) {
    base.mentions = mentionUris;
  }
}

function extractExplicitMentions(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const em = ext(msg).explicitMentions as { items?: Array<{ type?: string }> } | undefined;
  if (!em?.items) return;

  for (const m of em.items) {
    if (m.type === "everyone") { base.broadcastMention = "everyone"; return; }
    if (m.type === "here") { base.broadcastMention = "here"; return; }
  }
}

/** Callbacks the groupchat dispatcher invokes on the client. */
export interface GroupchatHandlers {
  currentRoom: string | null;
  selfNick: string;
  onMessage: ((msg: LiveRoomMessage) => void) | null;
  onChatState: ((event: ChatStateEvent) => void) | null;
  onDisplayed: ((event: DisplayedEvent) => void) | null;
  onReaction: ((event: ReactionEvent) => void) | null;
  onActivity: ((event: RoomActivityEvent) => void) | null;
}

/** Route an inbound groupchat message to the appropriate handler. */
export function dispatchGroupchat(msg: ReceivedMessage, h: GroupchatHandlers): void {
  const from = msg.from ?? "";
  const [roomJid, nick = "unknown"] = from.split("/");
  if (!roomJid) return;

  if (roomJid !== h.currentRoom) {
    if (msg.body) {
      const partial: LiveRoomMessage = { id: "", roomJid, nick, body: msg.body, createdAt: "", type: "message" };
      extractReferences(msg, partial);
      extractExplicitMentions(msg, partial);
      const activity: RoomActivityEvent = { roomJid, nick, body: msg.body };
      if (partial.mentions) activity.mentions = partial.mentions;
      if (partial.broadcastMention) activity.broadcastMention = partial.broadcastMention;
      h.onActivity?.(activity);
    }
    return;
  }

  if (nick !== h.selfNick && msg.chatState) {
    h.onChatState?.({ roomJid, nick, state: msg.chatState as ChatStateType });
  }

  const applyTo = ext(msg).applyTo as { id?: string; moderated?: { retract?: boolean } } | undefined;
  if (applyTo?.id && applyTo.moderated) {
    h.onMessage?.({ id: msg.id ?? crypto.randomUUID(), roomJid, nick, body: "", createdAt: new Date().toISOString(), type: "message", retractsId: applyTo.id });
    return;
  }

  const retract = ext(msg).retract as { id?: string } | undefined;
  if (retract?.id) {
    h.onMessage?.({ id: msg.id ?? crypto.randomUUID(), roomJid, nick, body: "", createdAt: new Date().toISOString(), type: "message", retractsId: retract.id });
    return;
  }

  if (msg.marker?.type === "displayed" && msg.marker.id && nick !== h.selfNick) {
    h.onDisplayed?.({ roomJid, nick, messageId: msg.marker.id });
    return;
  }

  const reactions = ext(msg).reactions as { id?: string; items?: string[] } | undefined;
  if (reactions?.id) {
    h.onReaction?.({ roomJid, nick, messageId: reactions.id, emojis: (reactions.items ?? []).filter((t) => t.length > 0) });
    return;
  }

  if (!msg.body && !msg.subject) return;

  const liveMsg: LiveRoomMessage = {
    id: msg.id ?? crypto.randomUUID(), roomJid, nick,
    body: msg.body ?? msg.subject ?? "",
    createdAt: new Date().toISOString(),
    type: msg.body ? "message" : "subject",
  };
  extractMessageExtensions(msg, liveMsg);
  h.onMessage?.(liveMsg);
}

function extractFileSharing(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const raw = ext(msg).fileSharing as
    | Array<{ disposition?: string; name?: string; mediaType?: string; size?: string; width?: string; height?: string; desc?: string; url?: string }>
    | { disposition?: string; name?: string; mediaType?: string; size?: string; width?: string; height?: string; desc?: string; url?: string }
    | undefined;
  if (!raw) return;
  const entries = Array.isArray(raw) ? raw : [raw];
  const rawEncrypted = ext(msg).encryptedFiles as WaddleEncryptedFile | WaddleEncryptedFile[] | undefined;
  const encryptedEntries = Array.isArray(rawEncrypted)
    ? rawEncrypted.filter((value): value is WaddleEncryptedFile => !!value)
    : rawEncrypted
      ? [rawEncrypted]
      : [];
  const encryptedBySourceUrl = new Map<string, WaddleEncryptedFile>();
  for (const encrypted of encryptedEntries) {
    for (const source of encrypted.sources ?? []) {
      if (source) encryptedBySourceUrl.set(source, encrypted);
    }
  }
  const out: SharedFileInfo[] = [];
  const useIndexFallback = encryptedEntries.length === entries.length;
  for (const [index, fs] of entries.entries()) {
    if (!fs?.url) continue;
    const info: SharedFileInfo = {
      url: fs.url,
      disposition: fs.disposition === "attachment" ? "attachment" : "inline",
    };
    if (fs.name) info.name = fs.name;
    if (fs.mediaType) info.mediaType = fs.mediaType;
    if (fs.size) info.size = parseInt(fs.size, 10);
    if (fs.width) info.width = parseInt(fs.width, 10);
    if (fs.height) info.height = parseInt(fs.height, 10);
    if (fs.desc) info.desc = fs.desc;
    const encrypted = encryptedBySourceUrl.get(fs.url) ?? (useIndexFallback ? encryptedEntries[index] : undefined);
    if (encrypted) info.encrypted = encrypted;
    out.push(info);
  }
  if (out.length > 0) base.sharedFiles = out;
}

/** XEP-0394: Extract Message Markup annotations. */
function extractMarkup(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const markupData = ext(msg).markup as { spans?: Array<{ type: string; start: number; end: number; uri?: string }> } | undefined;
  if (!markupData?.spans || markupData.spans.length === 0) return;

  base.markup = markupData.spans.map(s => ({
    type: s.type as import("@/lib/chat-ui").MarkupSpan["type"],
    start: s.start,
    end: s.end,
    ...(s.uri ? { uri: s.uri } : {}),
  }));
}

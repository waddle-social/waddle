import type { ExtensionAnnotation, ExtensionLaunchDescriptor, ExtensionPayloadElement } from "@/lib/chat-ui";
import type { MarkupSpan, MessageReference } from "@/lib/rich-message";
import type { RichInlineStyle } from "@/lib/rich-message/types";

import type { WaddleEncryptedFile } from "./extensions/encrypted-file";
import { barePeerJid } from "./jid";
import type { InboxEntry } from "./inbox-types";
import {
  buildReplyFallbackPrefix,
  shiftMarkupSpans,
  shiftReferenceOffsets,
  type ReplyTarget,
  type SendDirectMessageOptions,
  type SendGroupMessageOptions,
} from "./send-types";
import type { TimestampSource } from "@/lib/timeline-timestamps";
import type { LiveDmMessage, LiveRoomMessage, OccupantPresence, PresenceUpdateEvent, SharedFileInfo } from "./types";
import type {
  WasmArchivedMessage,
  WasmInboxConversation,
  WasmExtensionEnvelope,
  WasmExtensionPayloadElement,
  WasmMessage,
  WasmPresence,
  WasmReference,
  WasmSendOptions,
  WasmSharedFile,
} from "./wasm-types";

export function parsePresenceShow(show: string | undefined): OccupantPresence {
  switch (show) {
    case "away":
    case "xa":
      return "away";
    case "dnd":
      return "dnd";
    default:
      return "online";
  }
}

export function mapPresenceShow(presence: WasmPresence): PresenceUpdateEvent["show"] {
  if (presence.presence_type === "unavailable") return "offline";
  switch (presence.show ?? "available") {
    case "away":
      return "away";
    case "xa":
      return "xa";
    case "dnd":
      return "dnd";
    default:
      return "available";
  }
}

function bufferLikeToBase64(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export function avatarDataUrl(data: Uint8Array, mediaType?: string | null): string | null {
  if (!data?.length) return null;
  return `data:${mediaType || "image/png"};base64,${bufferLikeToBase64(data)}`;
}

function wasmSpanToMarkupSpan(span: WasmMessage["markup_spans"][number]): MarkupSpan | null {
  const styleMap: Record<string, RichInlineStyle> = {
    bold: "strong",
    italic: "emphasis",
    strikethrough: "deleted",
    code: "code",
  };
  const style = styleMap[span.span_type];
  if (style) return { type: "span", start: span.start, end: span.end, styles: [style] } satisfies MarkupSpan;
  if (span.span_type === "code_block") return { type: "bcode", start: span.start, end: span.end } satisfies MarkupSpan;
  if (span.span_type === "blockquote") return { type: "bquote", start: span.start, end: span.end } satisfies MarkupSpan;
  return null;
}

function rebaseOffsetAfterRemoval(offset: number, start: number, end: number): number {
  if (offset <= start) return offset;
  if (offset >= end) return offset - (end - start);
  return start;
}

function stripMarkupRange<T extends { start: number; end: number }>(spans: readonly T[], start: number, end: number): T[] {
  return spans.flatMap((span) => {
    const rebased = {
      ...span,
      start: rebaseOffsetAfterRemoval(span.start, start, end),
      end: rebaseOffsetAfterRemoval(span.end, start, end),
    };
    return rebased.end > rebased.start ? [rebased] : [];
  });
}

function stripReferenceRange<T extends { begin?: number; end?: number }>(references: readonly T[], start: number, end: number): T[] {
  return references.flatMap((reference) => {
    if (typeof reference.begin !== "number" || typeof reference.end !== "number") return [];
    // Anchor-only references (no body position) use the (0, 0) sentinel and
    // are unaffected by reply-fallback stripping, so preserve them verbatim.
    if (reference.begin === 0 && reference.end === 0) return [reference];
    const rebased = {
      ...reference,
      begin: rebaseOffsetAfterRemoval(reference.begin, start, end),
      end: rebaseOffsetAfterRemoval(reference.end, start, end),
    };
    return rebased.end > rebased.begin ? [rebased] : [];
  });
}

function stripReplyFallback<
  T extends {
    body: string;
    markup?: Array<{ start: number; end: number }>;
    references?: Array<{ begin?: number; end?: number }>;
  },
>(
  message: T,
  start?: number,
  end?: number,
): T {
  if (typeof start !== "number" || typeof end !== "number" || end <= start || !message.body) return message;
  const points = Array.from(message.body);
  const strippedBody = `${points.slice(0, start).join("")}${points.slice(end).join("")}`;
  const markup = message.markup?.length ? stripMarkupRange(message.markup, start, end) : undefined;
  const references = message.references?.length ? stripReferenceRange(message.references, start, end) : undefined;
  return {
    ...message,
    body: strippedBody,
    ...(markup?.length ? { markup } : {}),
    ...(references?.length ? { references } : {}),
  };
}

function sharedFileFromWasm(file: WasmSharedFile): SharedFileInfo {
  return {
    url: file.url,
    disposition: file.disposition === "attachment" ? "attachment" : "inline",
    ...(file.name ? { name: file.name } : {}),
    ...(file.media_type ? { mediaType: file.media_type } : {}),
    ...(typeof file.size === "number" ? { size: file.size } : {}),
    ...(typeof file.width === "number" ? { width: file.width } : {}),
    ...(typeof file.height === "number" ? { height: file.height } : {}),
    ...(file.encrypted ? { encrypted: encryptedFromWasm(file.encrypted) } : {}),
  };
}

function encryptedFromWasm(encrypted: NonNullable<WasmSharedFile["encrypted"]>): WaddleEncryptedFile {
  return {
    cipher: encrypted.cipher as WaddleEncryptedFile["cipher"],
    keyB64: encrypted.key_b64,
    ivB64: encrypted.iv_b64,
    ...(encrypted.hashes.length
      ? { hashes: encrypted.hashes.map((hash) => ({ algo: hash.algo, valueB64: hash.value_b64 })) }
      : {}),
    ...(encrypted.sources.length ? { sources: [...encrypted.sources] } : {}),
  };
}

function encryptedToWasm(encrypted: WaddleEncryptedFile, fallbackUrl: string): NonNullable<WasmSharedFile["encrypted"]> {
  return {
    cipher: encrypted.cipher,
    key_b64: encrypted.keyB64,
    iv_b64: encrypted.ivB64,
    hashes: (encrypted.hashes ?? []).map((hash) => ({ algo: hash.algo, value_b64: hash.valueB64 })),
    sources: encrypted.sources?.length ? [...encrypted.sources] : [fallbackUrl],
  };
}

function referenceFromWasm(reference: WasmReference): MessageReference {
  return {
    type: reference.ref_type,
    uri: reference.uri,
    begin: reference.begin,
    end: reference.end,
    ...(reference.anchor ? { anchor: reference.anchor } : {}),
  };
}

function payloadElementFromWasm(payload: WasmExtensionPayloadElement): ExtensionPayloadElement {
  return {
    namespace: payload.namespace,
    name: payload.name,
    attributes: Object.fromEntries(payload.attributes.map((attribute) => [attribute.name, attribute.value])),
    ...(payload.text ? { text: payload.text } : {}),
    children: payload.children.map(payloadElementFromWasm),
  };
}

function launchFromWasm(
  launch: WasmExtensionEnvelope["enrichments"][number]["launches"][number],
  source?: WasmExtensionEnvelope["enrichments"][number]["source"],
): ExtensionLaunchDescriptor | null {
  if (!launch.token || !launch.expires_at) return null;
  return {
    id: launch.id,
    pluginId: launch.plugin,
    actionId: launch.action,
    commandNode: launch.command_node,
    launchToken: launch.token,
    label: launch.label,
    context: {
      waddleId: launch.context.waddle_id,
      ...(launch.context.room ? { roomJid: launch.context.room } : {}),
      ...(launch.context.source_stanza_id ? { stanzaId: launch.context.source_stanza_id } : {}),
    },
    expiresAt: launch.expires_at,
    ...(source ? {
      source: {
        stanzaId: source.stanza_id,
        ...(source.body_start !== undefined ? { bodyStart: source.body_start } : {}),
        ...(source.body_end !== undefined ? { bodyEnd: source.body_end } : {}),
      },
    } : {}),
    payloads: launch.payloads.map(payloadElementFromWasm),
  };
}

// Allowed values for the `surface` declaration on the payload root.
// Extensions opt in by adding a `surface="..."` attribute; anything
// missing or unrecognised falls back to "message-card" so existing
// extensions keep their current treatment.
const KNOWN_SURFACE_KINDS = ["message-card", "board", "game", "chat-bot", "dynamic-canvas", "utility-panel"] as const;
type KnownSurfaceKind = typeof KNOWN_SURFACE_KINDS[number];

function surfaceKindFromPayloadAttributes(attrs: Record<string, string>): KnownSurfaceKind {
  const declared = attrs.surface;
  if (declared && (KNOWN_SURFACE_KINDS as readonly string[]).includes(declared)) {
    return declared as KnownSurfaceKind;
  }
  return "message-card";
}

function extensionAnnotationsFromWasm(envelope?: WasmExtensionEnvelope): ExtensionAnnotation[] {
  if (!envelope?.enrichments.length) return [];
  return envelope.enrichments.map((enrichment) => {
    const payloads = enrichment.payloads.map(payloadElementFromWasm);
    const fields = payloads[0]?.attributes ?? {};
    // Surface declaration travels with the payload root element. The
    // GitHub extension (and any future bot-style publisher) sets
    // `surface="chat-bot"` so the client renders it as a full-width
    // system band rather than a human-shaped message bubble. Default
    // is "message-card" — the existing treatment for everything that
    // doesn't opt in.
    const surfaceKind = surfaceKindFromPayloadAttributes(fields);
    const source = enrichment.source ? {
      stanzaId: enrichment.source.stanza_id,
      ...(enrichment.source.body_start !== undefined ? { bodyStart: enrichment.source.body_start } : {}),
      ...(enrichment.source.body_end !== undefined ? { bodyEnd: enrichment.source.body_end } : {}),
    } : undefined;
    const actions = enrichment.launches.flatMap((launch) => {
      const launchDescriptor = launchFromWasm(launch, enrichment.source);
      return launchDescriptor
        ? [{ label: launch.label, route: launch.id, launch: launchDescriptor }]
        : [];
    });
    return {
      extensionId: enrichment.plugin,
      annotationId: enrichment.id,
      surfaceKind,
      title: enrichment.title?.trim() || enrichment.plugin,
      ...(enrichment.summary?.trim() ? { summary: enrichment.summary.trim() } : {}),
      ...(source ? { source } : {}),
      payloadNamespace: enrichment.payload_namespace,
      payloads,
      fields: {
        surface: surfaceKind,
        payloadNamespace: enrichment.payload_namespace,
        ...fields,
      },
      actions,
    };
  });
}

/**
 * Codec source distinguishes a true MAM archive result from a live
 * wire stanza that was reshaped into the archived struct via
 * `{ ...message, mam_id: ... }`. The two cases share a codec but
 * carry different timestamp authority — see `TimestampSource`.
 */
type CodecSource = "archive" | "live";

function deriveCreatedAt(
  rawTimestamp: string | undefined | null,
  source: CodecSource,
): { createdAt: string; createdAtSource: TimestampSource } {
  if (typeof rawTimestamp === "string" && rawTimestamp.length > 0) {
    return {
      createdAt: rawTimestamp,
      createdAtSource: source === "archive" ? "archive" : "delay",
    };
  }
  // Live wire stanza without a `<delay/>` element — the message is
  // genuinely arriving "now" in most cases (peer just sent it), so
  // `Date.now()` is a sensible best-effort. For redelivered stanzas
  // (XEP-0198 resume tail, carbons that dropped delay, MUC join
  // history) this is wrong, but a later MAM hit with `archive` /
  // `delay` authority will replace it via `pickAuthoritativeTimestamp`.
  return {
    createdAt: new Date().toISOString(),
    createdAtSource: "fallback",
  };
}

function roomAssignedStanzaId(message: WasmArchivedMessage, roomJid: string): string | undefined {
  const byScoped = message.stanza_ids?.find((stanzaId) => stanzaId.by === roomJid);
  if (byScoped?.id) return byScoped.id;
  return message.stanza_id && message.stanza_id_by === roomJid
    ? message.stanza_id
    : undefined;
}

export function roomMessageFromArchived(
  message: WasmArchivedMessage,
  source: CodecSource = "archive",
): LiveRoomMessage | null {
  const fromJid = message.from ?? "";
  const roomJid = barePeerJid(fromJid || message.to || "");
  const nick = fromJid.split("/")[1] ?? "unknown";
  const { createdAt, createdAtSource } = deriveCreatedAt(message.timestamp, source);
  const stampedByRoom = roomAssignedStanzaId(message, roomJid);
  const moderationTargetId = message.moderation_target_id && fromJid === roomJid
    ? message.moderation_target_id
    : undefined;
  const activeRetractionTarget = moderationTargetId ?? (!message.moderation_target_id ? message.retracts_id : undefined);
  const roomPrimaryId = message.id ?? stampedByRoom ?? message.mam_id;
  const roomWireIds = [message.id, message.origin_id, stampedByRoom]
    .filter((value): value is string => !!value && value !== roomPrimaryId);
  if (activeRetractionTarget) {
    return {
      id: roomPrimaryId,
      archiveId: message.mam_id,
      fromJid,
      roomJid,
      nick,
      body: "",
      createdAt,
      createdAtSource,
      type: "message",
      retractsId: activeRetractionTarget,
      ...(message.retraction_id ? { retractionId: message.retraction_id } : {}),
      ...(moderationTargetId ? { moderationTargetId } : {}),
      ...(message.moderated_by ? { moderatedBy: message.moderated_by } : {}),
      ...(message.moderation_reason ? { moderationReason: message.moderation_reason } : {}),
      ...(message.author_real_jid ? { authorRealJid: message.author_real_jid } : {}),
    };
  }
  if (message.reaction_target_id) {
    return {
      id: roomPrimaryId,
      archiveId: message.mam_id,
      roomJid,
      nick,
      body: "",
      createdAt,
      createdAtSource,
      type: "subject",
      _reactionTarget: message.reaction_target_id,
      _reactionEmojis: message.reaction_emojis,
      ...(message.author_real_jid ? { _reactionSenderId: message.author_real_jid } : {}),
    };
  }
  // #414: pin-event system messages from the room bare JID render
  // distinctly so users see "alice pinned a message" inline without
  // it looking like a normal user post.
  if (message.pin_event) {
    return {
      id: message.id ?? message.stanza_id ?? message.mam_id,
      archiveId: message.mam_id,
      roomJid,
      nick: "",
      body: message.body ?? "",
      createdAt,
      createdAtSource,
      type: "pin-event",
      pinEventAction: message.pin_event.action,
    };
  }
  const sharedFiles = message.shared_files ?? [];
  const mentionUris = message.mention_uris ?? [];
  const markupSpans = message.markup_spans ?? [];
  const references = message.references ?? [];
  const extensionAnnotations = extensionAnnotationsFromWasm(message.extension_envelope);
  // XEP-0201 `<thread/>` is scope metadata, not content. A stanza that
  // carries only a thread reference (no body, no subject, no attachments,
  // no extension annotations, no forum-post payload, not a retraction)
  // is a chat-state / displayed-marker / etc. echoed inside a thread per
  // XEP-0201 §3 — it MUST drop here so it never materialises as an empty
  // row inside the thread panel. The server-side `is_archivable` already
  // refuses to persist these; this is the defence-in-depth pass for
  // foreign servers and legacy archive rows.
  if (!message.body && !message.subject && !sharedFiles.length && !extensionAnnotations.length && !message.forum_post_kind && !message.is_retracted) {
    return null;
  }
  const base: LiveRoomMessage = {
    id: roomPrimaryId,
    archiveId: message.mam_id,
    fromJid,
    roomJid,
    nick,
    body: message.is_retracted ? "" : (message.body ?? message.subject ?? ""),
    createdAt,
    createdAtSource,
    type: message.subject && !message.body ? "subject" : "message",
    ...(message.replaces_id ? { replacesId: message.replaces_id } : {}),
    ...(roomWireIds.length > 0
      ? { wireIds: roomWireIds }
      : {}),
    ...(message.reply_to_id
      ? { replyTo: { id: message.reply_to_id, ...(message.reply_to_sender ? { author: message.reply_to_sender } : {}) } }
      : {}),
    ...(message.thread ? { threadId: message.thread } : {}),
    ...(message.parent_thread_id ? { parentThreadId: message.parent_thread_id } : {}),
    ...(message.forum_post_kind === "topic" || message.forum_post_kind === "reply" ? { forumPostKind: message.forum_post_kind } : {}),
    ...(message.forum_title ? { forumTitle: message.forum_title } : {}),
    ...(message.forum_thread_title ? { forumThreadTitle: message.forum_thread_title } : {}),
    ...(message.author_real_jid ? { authorRealJid: message.author_real_jid } : {}),
    ...(stampedByRoom ? { reactionTargetId: stampedByRoom } : {}),
    ...(mentionUris.length ? { mentions: mentionUris.map((uri) => uri.replace(/^xmpp:/, "")) } : {}),
    ...(message.broadcast_mention === "here" || message.broadcast_mention === "everyone" ? { broadcastMention: message.broadcast_mention } : {}),
    ...(markupSpans.length
      ? { markup: markupSpans.flatMap((s) => { const m = wasmSpanToMarkupSpan(s); return m ? [m] : []; }) }
      : {}),
    ...(references.length ? { references: references.map(referenceFromWasm) } : {}),
    ...(sharedFiles.length ? { sharedFiles: sharedFiles.map(sharedFileFromWasm) } : {}),
    ...(extensionAnnotations.length ? { extensionAnnotations } : {}),
    ...(message.extension_body_fallback && extensionAnnotations.length ? { extensionBodyFallback: true } : {}),
    ...(message.is_sticker ? { isSticker: true } : {}),
    ...(message.is_retracted ? { isRetracted: true } : {}),
    ...(message.retraction_id ? { retractionId: message.retraction_id } : {}),
    ...(message.moderated_by ? { moderatedBy: message.moderated_by } : {}),
    ...(message.moderation_reason ? { moderationReason: message.moderation_reason } : {}),
    ...(message.stanza_id ?? message.origin_id ? { correctionTargetId: message.origin_id ?? message.id ?? "" } : {}),
    ...(stampedByRoom ? { replyableId: stampedByRoom } : {}),
    // XEP-0490 needs the room-injected stanza-id (by=room) so the
    // MDS publish carries the spec-correct stanza-id for group chats.
    ...(stampedByRoom ? { stanzaId: stampedByRoom, stanzaIdBy: roomJid } : {}),
  };
  return stripReplyFallback(base, message.reply_fallback_start, message.reply_fallback_end);
}

export function dmMessageFromArchived(
  message: WasmArchivedMessage,
  selfBareJid: string,
  source: CodecSource = "archive",
): LiveDmMessage | null {
  const fromBare = barePeerJid(message.from ?? "");
  const toBare = barePeerJid(message.to ?? "");
  const isSelf = fromBare === selfBareJid;
  const peerJid = isSelf ? toBare : fromBare;
  if (!peerJid) return null;
  const fromJid = message.from ?? fromBare;
  const nick = barePeerJid(fromJid).split("@")[0] ?? "unknown";
  const { createdAt, createdAtSource } = deriveCreatedAt(message.timestamp, source);
  const dmPrimaryId = message.id ?? message.origin_id ?? message.mam_id;
  if (message.retracts_id) {
    return {
      id: dmPrimaryId,
      archiveId: message.mam_id,
      peerJid,
      fromJid,
      nick,
      body: "",
      createdAt,
      createdAtSource,
      type: "message",
      retractsId: message.retracts_id,
      ...(message.retraction_id ? { retractionId: message.retraction_id } : {}),
    };
  }
  if (message.reaction_target_id) {
    return {
      id: dmPrimaryId,
      archiveId: message.mam_id,
      peerJid,
      fromJid,
      nick,
      body: "",
      createdAt,
      createdAtSource,
      type: "message",
      _reactionTarget: message.reaction_target_id,
      _reactionEmojis: message.reaction_emojis,
    };
  }
  const sharedFiles = message.shared_files ?? [];
  const mentionUris = message.mention_uris ?? [];
  const markupSpans = message.markup_spans ?? [];
  const references = message.references ?? [];
  const extensionAnnotations = extensionAnnotationsFromWasm(message.extension_envelope);
  const dmWireIds = [message.id, message.origin_id]
    .filter((value): value is string => !!value && value !== dmPrimaryId);
  // See `roomMessageFromArchived` for the rationale: `<thread/>` alone is
  // metadata, not content. DMs don't currently surface threads in the UI
  // anyway, but the codec stays symmetric with the room path so a stray
  // foreign-server archive row can't manifest a bodyless DM ghost either.
  if (!message.body && !message.subject && !sharedFiles.length && !extensionAnnotations.length && !message.is_retracted) return null;
  const base: LiveDmMessage = {
    id: dmPrimaryId,
    archiveId: message.mam_id,
    peerJid,
    fromJid,
    nick,
    body: message.is_retracted ? "" : (message.body ?? message.subject ?? ""),
    createdAt,
    createdAtSource,
    type: "message",
    ...(message.replaces_id ? { replacesId: message.replaces_id } : {}),
    ...(message.reply_to_id
      ? { replyTo: { id: message.reply_to_id, ...(message.reply_to_sender ? { author: message.reply_to_sender } : {}) } }
      : {}),
    ...(message.thread ? { threadId: message.thread } : {}),
    ...(message.parent_thread_id ? { parentThreadId: message.parent_thread_id } : {}),
    ...(message.forum_post_kind === "topic" || message.forum_post_kind === "reply" ? { forumPostKind: message.forum_post_kind } : {}),
    ...(message.forum_title ? { forumTitle: message.forum_title } : {}),
    ...(message.forum_thread_title ? { forumThreadTitle: message.forum_thread_title } : {}),
    ...(mentionUris.length ? { mentions: mentionUris.map((uri) => uri.replace(/^xmpp:/, "")) } : {}),
    ...(markupSpans.length
      ? { markup: markupSpans.flatMap((s) => { const m = wasmSpanToMarkupSpan(s); return m ? [m] : []; }) }
      : {}),
    ...(references.length ? { references: references.map(referenceFromWasm) } : {}),
    ...(sharedFiles.length ? { sharedFiles: sharedFiles.map(sharedFileFromWasm) } : {}),
    ...(extensionAnnotations.length ? { extensionAnnotations } : {}),
    ...(message.extension_body_fallback && extensionAnnotations.length ? { extensionBodyFallback: true } : {}),
    ...(message.is_sticker ? { isSticker: true } : {}),
    ...(message.is_retracted ? { isRetracted: true } : {}),
    ...(message.retraction_id ? { retractionId: message.retraction_id } : {}),
    ...(message.origin_id || message.id ? { correctionTargetId: message.origin_id ?? message.id ?? "" } : {}),
    ...(message.origin_id || message.id ? { replyableId: message.origin_id ?? message.id ?? undefined } : {}),
    // XEP-0490 needs the user-server-injected stanza-id for DM
    // displayed-sync. `message.stanza_id` plus its `by` JID round-
    // trip from the wasm parser.
    ...(message.stanza_id ? { stanzaId: message.stanza_id } : {}),
    ...(message.stanza_id_by ? { stanzaIdBy: message.stanza_id_by } : {}),
    ...(dmWireIds.length > 0
      ? { wireIds: dmWireIds }
      : {}),
  };
  return stripReplyFallback(base, message.reply_fallback_start, message.reply_fallback_end);
}

export function inboxEntryFromWasm(conversation: WasmInboxConversation): InboxEntry {
  return {
    partner: conversation.partner,
    kind: conversation.kind === "muc" ? "muc" : "direct",
    lastStanzaId: conversation.last_stanza_id,
    lastUpdated: conversation.last_updated,
    unread: conversation.unread,
    ...(conversation.preview ? { preview: conversation.preview } : {}),
    ...(conversation.thread ? { thread: conversation.thread } : {}),
    ...(conversation.thread_title ? { threadTitle: conversation.thread_title } : {}),
    ...(typeof conversation.reply_count === "number" ? { replyCount: conversation.reply_count } : {}),
    ...(conversation.author ? { author: conversation.author } : {}),
  };
}

export function buildWasmSendOptions(
  opts: SendGroupMessageOptions | SendDirectMessageOptions,
  replyFallbackLength: number,
): WasmSendOptions {
  const wasmOpts: WasmSendOptions = {};
  const maybeGroupOpts = opts as SendGroupMessageOptions;
  const generatedId = opts.id ?? crypto.randomUUID();
  wasmOpts.stanza_id = generatedId;
  if (opts.replyTo) {
    wasmOpts.reply = { author_jid: opts.replyTo.author, message_id: opts.replyTo.id };
    if (replyFallbackLength > 0) wasmOpts.fallback = { start: 0, end: replyFallbackLength };
  }
  if (maybeGroupOpts.threadCreate) {
    wasmOpts.subject = maybeGroupOpts.threadCreate.title;
    wasmOpts.thread = { id: generatedId, parent: maybeGroupOpts.parentThreadId };
  } else if (maybeGroupOpts.threadReply) {
    wasmOpts.thread = { id: maybeGroupOpts.threadReply.threadId, parent: maybeGroupOpts.parentThreadId };
  } else if (opts.threadId) {
    wasmOpts.thread = { id: opts.threadId, ...(opts.parentThreadId ? { parent: opts.parentThreadId } : {}) };
  }
  if (opts.files?.length) {
    wasmOpts.shared_files = opts.files.map((file) => ({
      url: file.url,
      name: file.name,
      media_type: file.mediaType,
      size: file.size,
      width: file.width,
      height: file.height,
      disposition: file.disposition,
      ...(file.encrypted ? { encrypted: encryptedToWasm(file.encrypted, file.url) } : {}),
    }));
  }
  if (opts.markup?.length) {
    const styleToWasm: Record<RichInlineStyle, string> = { strong: "bold", emphasis: "italic", deleted: "strikethrough", code: "code" };
    wasmOpts.markup_spans = opts.markup.flatMap((span) => {
      if (span.type === "span") return span.styles.map((style) => ({ span_type: styleToWasm[style] ?? style, start: span.start, end: span.end }));
      if (span.type === "bcode") return [{ span_type: "code_block", start: span.start, end: span.end }];
      if (span.type === "bquote") return [{ span_type: "blockquote", start: span.start, end: span.end }];
      return [];
    });
  }
  if (opts.references?.length) {
    wasmOpts.references = opts.references.map((reference) => ({
      ref_type: reference.type ?? "mention",
      uri: reference.uri ?? "",
      begin: reference.begin ?? 0,
      end: reference.end ?? 0,
      ...(reference.anchor ? { anchor: reference.anchor } : {}),
    }));
  }
  return wasmOpts;
}

export function encodeBodyForSend(
  body: string,
  replyTo?: ReplyTarget,
  markup?: SendGroupMessageOptions["markup"],
  references?: SendGroupMessageOptions["references"],
) {
  const { prefix, length } = replyTo ? buildReplyFallbackPrefix(replyTo.body) : { prefix: "", length: 0 };
  return {
    effectiveBody: `${prefix}${body}`,
    replyFallbackLength: length,
    rebasedMarkup: markup?.length ? shiftMarkupSpans(markup, length) : undefined,
    rebasedReferences: references?.length ? shiftReferenceOffsets(references, length) : undefined,
  };
}

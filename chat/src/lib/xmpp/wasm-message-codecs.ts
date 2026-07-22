import type { ExtensionAnnotation, ExtensionLaunchDescriptor, ExtensionPayloadElement, LinkPreview } from "@/lib/chat-ui";
import type { MarkupSpan, MessageReference } from "@/lib/rich-message";
import type { RichInlineStyle } from "@/lib/rich-message/types";

import type { WaddleEncryptedFile } from "./extensions/encrypted-file";
import { bareJidKey, barePeerJid, jidDomain, jidLocalpart } from "./jid";
import type { InboxEntry } from "./inbox-types";
import { isTrustedCachedPreviewImageUrl } from "./link-preview";
import { isAllowedPlayerEmbedOrigin } from "@/lib/xmpp/player-embed-allowlist";
import { associateDirectVideoPreviews } from "./link-preview-video";
import { isPlayableNativeVideo } from "./native-video";
import {
  buildReplyFallbackPrefix,
  shiftMarkupSpans,
  shiftReferenceOffsets,
  type ReplyTarget,
  type SendDirectMessageOptions,
  type SendGroupMessageOptions,
} from "./send-types";
import type { TimestampSource } from "@/lib/timeline-timestamps";
import { dmCallAnchorId } from "../calls/dm-call-anchor";
import type { LiveDmMessage, LiveRoomMessage, OccupantPresence, PresenceUpdateEvent, SharedFileInfo } from "./types";
import type {
  WasmArchivedMessage,
  WasmCallThreadAnchor,
  WasmInboxConversation,
  WasmExtensionEnvelope,
  WasmExtensionPayloadElement,
  WasmMessage,
  WasmPresence,
  WasmReference,
  WasmSendOptions,
  WasmLinkPreview,
  WasmSharedFile,
} from "./wasm-types";

export function parsePresenceShow(show: string | undefined): OccupantPresence {
  switch (show) {
    case "away":
    case "xa":
      return "away";
    case "dnd":
      return "dnd";
    // RFC 6121 `chat` ("free for chat") folds into online — an available
    // substate, rendered the same as plain Available (ADR-010 Phase 5b).
    case "chat":
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
    // RFC 6121 `chat` ("free for chat") folds into available — an available
    // substate, rendered the same as plain Available (ADR-010 Phase 5b).
    case "chat":
    default:
      return "available";
  }
}

/**
 * XEP-0319 `<idle since>` → epoch ms for {@link PresenceUpdateEvent.idleSince}.
 * Returns `undefined` when absent or when the wire value isn't a valid
 * xs:dateTime, so a malformed stamp degrades to "no idle" rather than NaN.
 */
export function mapPresenceIdleSince(presence: WasmPresence): number | undefined {
  if (!presence.idle_since) return undefined;
  const ms = Date.parse(presence.idle_since);
  return Number.isNaN(ms) ? undefined : ms;
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

function isSupportedRoomCallThread(
  anchor: WasmCallThreadAnchor | undefined,
): anchor is WasmCallThreadAnchor & { kind: "muc" } {
  return anchor?.kind === "muc";
}

function isSupportedDmCallThread(
  anchor: WasmCallThreadAnchor | undefined,
): anchor is WasmCallThreadAnchor & { kind: "dm" } {
  return anchor?.kind === "dm";
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

interface LinkPreviewDecodeOptions {
  trustedMediaOrigin?: string | null;
}

export function linkPreviewFromWasm(preview: WasmLinkPreview, options: LinkPreviewDecodeOptions = {}): LinkPreview {
  const trustedImage = preview.image && isTrustedCachedPreviewImageUrl(preview.image.url, options.trustedMediaOrigin)
    ? preview.image
    : undefined;
  const remoteMediaUnavailable = !!preview.remote_media_unavailable || (!!preview.image && !trustedImage);
  const playerEmbed = preview.player_embed && isAllowedPlayerEmbedOrigin(preview.player_embed.url)
    ? preview.player_embed
    : undefined;
  const nativeVideo = preview.video && isPlayableNativeVideo(preview.video.url, preview.video.media_type)
    ? preview.video
    : undefined;
  return {
    originalUrl: preview.original_url,
    ...(preview.normalized_url ? { normalizedUrl: preview.normalized_url } : {}),
    ...(preview.title ? { title: preview.title } : {}),
    ...(preview.description ? { description: preview.description } : {}),
    ...(remoteMediaUnavailable ? { remoteMediaUnavailable: true } : {}),
    ...(trustedImage
      ? {
          image: {
            url: trustedImage.url,
            mediaType: trustedImage.media_type,
            ...(typeof trustedImage.width === "number" ? { width: trustedImage.width } : {}),
            ...(typeof trustedImage.height === "number" ? { height: trustedImage.height } : {}),
            ...(trustedImage.alt ? { alt: trustedImage.alt } : {}),
          },
        }
      : {}),
    ...(nativeVideo
      ? {
          video: {
            url: nativeVideo.url,
            mediaType: nativeVideo.media_type,
          },
        }
      : {}),
    ...(playerEmbed
      ? {
          playerEmbed: {
            url: playerEmbed.url,
            ...(typeof playerEmbed.width === "number" ? { width: playerEmbed.width } : {}),
            ...(typeof playerEmbed.height === "number" ? { height: playerEmbed.height } : {}),
          },
        }
      : {}),
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

/**
 * XEP-0359 `by` comparison key: case-folded bare JID (RFC 7622), but a
 * resource-carrying `by` never matches — a resource-scoped value is a
 * different entity than the archiving authority, and folding it away
 * would let `room@conf/nick` impersonate the room. Mirrors the seen-id
 * path (`stanzaIdAuthorityKey` in client-mam.ts); the two MUST agree.
 */
export function stanzaIdAuthorityKey(value: string): string | null {
  if (!value || value.includes("/")) return null;
  return bareJidKey(value);
}

function roomAssignedStanzaId(message: WasmArchivedMessage, roomJid: string): string | undefined {
  const room = stanzaIdAuthorityKey(roomJid);
  if (!room) return undefined;
  const byScoped = message.stanza_ids?.find((stanzaId) => stanzaIdAuthorityKey(stanzaId.by) === room);
  if (byScoped?.id) return byScoped.id;
  return message.stanza_id && message.stanza_id_by && stanzaIdAuthorityKey(message.stanza_id_by) === room
    ? message.stanza_id
    : undefined;
}

function assignedStanzaIdBy(
  message: WasmArchivedMessage,
  authorities: readonly string[],
): { id: string; by: string } | null {
  // Same case-folded (RFC 7622) authority comparison as
  // `rawMessageSeenIds` — the seen-id path and the row-identity path
  // must accept exactly the same stanza-ids, or a mixed-case `by`
  // (e.g. "Example.COM") enters seen-ids but not the row's wireIds and
  // the archive copy re-emits as a duplicate on reconnect.
  const normalizedAuthorities = authorities
    .map(stanzaIdAuthorityKey)
    .filter((value): value is string => !!value);
  for (const authority of normalizedAuthorities) {
    const byScoped = message.stanza_ids?.find(
      (stanzaId) => stanzaId.id && stanzaIdAuthorityKey(stanzaId.by) === authority,
    );
    // Return the NORMALIZED authority, never the wire-cased original:
    // a mixed-case `by` (e.g. "Example.COM") propagating into
    // `stanzaIdBy` would fail strict-equality comparisons downstream
    // (MDS displayed-sync matching). Normalize once, at the source.
    if (byScoped?.id) return { id: byScoped.id, by: authority };
    if (
      message.stanza_id &&
      message.stanza_id_by &&
      stanzaIdAuthorityKey(message.stanza_id_by) === authority
    ) {
      return { id: message.stanza_id, by: authority };
    }
  }
  return null;
}

export function roomMessageFromArchived(
  message: WasmArchivedMessage,
  sourceOrOptions: CodecSource | LinkPreviewDecodeOptions = "archive",
  maybeOptions: LinkPreviewDecodeOptions = {},
): LiveRoomMessage | null {
  const source = typeof sourceOrOptions === "string" ? sourceOrOptions : "archive";
  const decodeOptions = typeof sourceOrOptions === "string" ? maybeOptions : sourceOrOptions;
  const fromJid = message.from ?? "";
  const roomJid = barePeerJid(fromJid || message.to || "");
  const nick = fromJid.split("/")[1] ?? "unknown";
  const { createdAt, createdAtSource } = deriveCreatedAt(message.timestamp, source);
  const stampedByRoom = roomAssignedStanzaId(message, roomJid);
  const moderationTargetId = message.moderation_target_id && fromJid === roomJid
    ? message.moderation_target_id
    : undefined;
  const activeRetractionTarget = moderationTargetId ?? (!message.moderation_target_id ? message.retracts_id : undefined);
  // XEP-0359 sender-selected ids are aliases, never room-global identity.
  // Prefer the room-authored stanza-id; without one, use the archive/live
  // envelope id and retain client id/origin-id only for sender-scoped merge.
  const roomPrimaryId = stampedByRoom ?? message.mam_id;
  const roomWireIds = [message.id, message.origin_id, stampedByRoom]
    .filter((value): value is string => !!value && value !== roomPrimaryId);
  // #1182: on the live path `mam_id` is fabricated by the dispatcher
  // (`message.id ?? randomUUID`), so a stanza with no wire identity at all
  // ends up keyed on a random UUID that the MAM copy can never match. Flag
  // it for content-based reconciliation and don't let the fabricated UUID
  // masquerade as an archive UID. `!message.id` is essential: a client-id-
  // only stanza also satisfies `primary === mam_id` but has a real identity.
  const synthesizedPrimary =
    source === "live" && !message.id && roomPrimaryId === message.mam_id && roomWireIds.length === 0;
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
      // #1267 item 3: only trust a stanza-id whose `by` is the room
      // (XEP-0359 §Security) — never the unverified first-listed one.
      id: roomPrimaryId,
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
  if (message.call_thread_ended) {
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
      callThreadEnded: {
        anchorId: message.call_thread_ended.anchor_id,
        ended: message.call_thread_ended.ended,
        duration: message.call_thread_ended.duration,
      },
      ...(stampedByRoom ? { stanzaId: stampedByRoom, stanzaIdBy: roomJid } : {}),
    };
  }
  const sharedFiles = message.shared_files ?? [];
  const linkPreviews = message.link_previews ?? [];
  const mentionUris = message.mention_uris ?? [];
  const markupSpans = message.markup_spans ?? [];
  const references = message.references ?? [];
  const extensionAnnotations = extensionAnnotationsFromWasm(message.extension_envelope);
  const callThread =
    message.body && isSupportedRoomCallThread(message.call_thread)
      ? message.call_thread
      : undefined;
  // XEP-0201 `<thread/>` is scope metadata, not content. A stanza that
  // carries only a thread reference (no body, no subject, no attachments,
  // no link preview, no extension annotations, no forum-post payload, not a retraction)
  // is a chat-state / displayed-marker / etc. echoed inside a thread per
  // XEP-0201 §3 — it MUST drop here so it never materialises as an empty
  // row inside the thread panel. The server-side `is_archivable` already
  // refuses to persist these; this is the defence-in-depth pass for
  // foreign servers and legacy archive rows.
  if (!message.body && !message.subject && !callThread && !sharedFiles.length && !linkPreviews.length && !extensionAnnotations.length && !message.forum_post_kind && !message.is_retracted) {
    return null;
  }
  const { linkPreviews: associatedLinkPreviews, sharedFiles: associatedSharedFiles } =
    associateDirectVideoPreviews(
      linkPreviews.map((preview) => linkPreviewFromWasm(preview, decodeOptions)),
      sharedFiles.map(sharedFileFromWasm),
    );
  const base: LiveRoomMessage = {
    id: roomPrimaryId,
    ...(synthesizedPrimary ? { synthesizedId: true } : { archiveId: message.mam_id }),
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
    ...(callThread
      ? {
          callThread: {
            kind: "muc",
            sid: callThread.sid,
            media: callThread.media,
            initiator: callThread.initiator,
            started: callThread.started,
          },
        }
      : {}),
    ...(message.author_real_jid ? { authorRealJid: message.author_real_jid } : {}),
    ...(stampedByRoom ? { reactionTargetId: stampedByRoom } : {}),
    ...(mentionUris.length ? { mentions: mentionUris.map((uri) => uri.replace(/^xmpp:/, "")) } : {}),
    ...(message.broadcast_mention === "here" || message.broadcast_mention === "everyone" ? { broadcastMention: message.broadcast_mention } : {}),
    ...(markupSpans.length
      ? { markup: markupSpans.flatMap((s) => { const m = wasmSpanToMarkupSpan(s); return m ? [m] : []; }) }
      : {}),
    ...(references.length ? { references: references.map(referenceFromWasm) } : {}),
    ...(associatedSharedFiles.length ? { sharedFiles: associatedSharedFiles } : {}),
    ...(associatedLinkPreviews.length ? { linkPreviews: associatedLinkPreviews } : {}),
    ...(extensionAnnotations.length ? { extensionAnnotations } : {}),
    ...(message.extension_body_fallback && extensionAnnotations.length ? { extensionBodyFallback: true } : {}),
    ...(message.is_sticker ? { isSticker: true } : {}),
    ...(message.is_retracted ? { isRetracted: true } : {}),
    ...(message.retraction_id ? { retractionId: message.retraction_id } : {}),
    ...(message.moderated_by ? { moderatedBy: message.moderated_by } : {}),
    ...(message.moderation_reason ? { moderationReason: message.moderation_reason } : {}),
    // #1267 item 3 + Qodo: a correction target must be an id the sender
    // actually owns (origin/wire id) and never an empty string — a
    // room-verified stanza-id feeds replies/MDS below, not corrections.
    ...(message.origin_id ?? message.id ? { correctionTargetId: (message.origin_id ?? message.id) as string } : {}),
    ...(stampedByRoom ? { replyableId: stampedByRoom } : {}),
    // XEP-0490 needs the room-injected stanza-id (by=room) so the
    // MDS publish carries the spec-correct stanza-id for group chats.
    ...(stampedByRoom ? { stanzaId: stampedByRoom, stanzaIdBy: roomJid } : {}),
    ...(message.displayed_marker_requested ? { displayedMarkerRequested: true } : {}),
  };
  return stripReplyFallback(base, message.reply_fallback_start, message.reply_fallback_end);
}

export function dmMessageFromArchived(
  message: WasmArchivedMessage,
  selfBareJid: string,
  sourceOrOptions: CodecSource | LinkPreviewDecodeOptions = "archive",
  maybeOptions: LinkPreviewDecodeOptions = {},
): LiveDmMessage | null {
  const source = typeof sourceOrOptions === "string" ? sourceOrOptions : "archive";
  const decodeOptions = typeof sourceOrOptions === "string" ? maybeOptions : sourceOrOptions;
  const fromBare = barePeerJid(message.from ?? "");
  const toBare = barePeerJid(message.to ?? "");
  const isSelf = fromBare === selfBareJid;
  const peerJid = isSelf ? toBare : fromBare;
  if (!peerJid) return null;
  const ownStanzaId = assignedStanzaIdBy(message, [jidDomain(selfBareJid), selfBareJid]);
  const fromJid = message.from ?? fromBare;
  const nick = jidLocalpart(fromJid);
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
  const linkPreviews = message.link_previews ?? [];
  const mentionUris = message.mention_uris ?? [];
  const markupSpans = message.markup_spans ?? [];
  const references = message.references ?? [];
  const extensionAnnotations = extensionAnnotationsFromWasm(message.extension_envelope);
  const callThread = isSupportedDmCallThread(message.call_thread) ? message.call_thread : undefined;
  const callThreadEnded = message.call_thread_ended;
  // #1182: the user-server-injected XEP-0359 stanza-id is a real wire
  // identity — the MAM copy is keyed on it (XEP-0313 servers use it as the
  // archive UID) — so fold it into the aliases like the room path does
  // with `stampedByRoom`. This both dedupes exactly and keeps such rows
  // out of the synthesized-id content-match pool.
  const dmWireIds = [message.id, message.origin_id, ownStanzaId?.id]
    .filter((value): value is string => !!value && value !== dmPrimaryId);
  // Alias a DM call-thread anchor by its sid so a same-session MAM backfill
  // collapses onto the synthesized live anchor (see `dm-call-anchor.ts`)
  // instead of duplicating the "started a call" card.
  if (callThread?.sid) {
    const anchorAlias = dmCallAnchorId(callThread.sid);
    if (anchorAlias !== dmPrimaryId && !dmWireIds.includes(anchorAlias)) {
      dmWireIds.push(anchorAlias);
    }
  }
  // See `roomMessageFromArchived` for the rationale: `<thread/>` alone is
  // metadata, not content. DMs don't currently surface threads in the UI
  // anyway, but the codec stays symmetric with the room path so a stray
  // foreign-server archive row can't manifest a bodyless DM ghost either.
  if (!message.body && !message.subject && !callThread && !sharedFiles.length && !linkPreviews.length && !extensionAnnotations.length && !message.is_retracted) return null;
  // #1182: see `roomMessageFromArchived` — a live stanza with no wire
  // identity is keyed on a dispatcher-fabricated UUID; flag it for
  // content-based reconciliation instead of claiming an archive UID.
  // `!message.id` is essential (client-id-only also has primary === mam_id).
  const synthesizedPrimary =
    source === "live" && !message.id && dmPrimaryId === message.mam_id && dmWireIds.length === 0;
  const { linkPreviews: associatedLinkPreviews, sharedFiles: associatedSharedFiles } =
    associateDirectVideoPreviews(
      linkPreviews.map((preview) => linkPreviewFromWasm(preview, decodeOptions)),
      sharedFiles.map(sharedFileFromWasm),
    );
  const base: LiveDmMessage = {
    id: dmPrimaryId,
    ...(synthesizedPrimary ? { synthesizedId: true } : { archiveId: message.mam_id }),
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
    ...(callThread
      ? {
          callThread: {
            kind: "dm",
            sid: callThread.sid,
            media: callThread.media,
            initiator: callThread.initiator,
            started: callThread.started,
            ...(callThreadEnded
              ? {
                  ended: callThreadEnded.ended,
                  duration: callThreadEnded.duration,
                }
              : {}),
          },
        }
      : {}),
    ...(message.forum_post_kind === "topic" || message.forum_post_kind === "reply" ? { forumPostKind: message.forum_post_kind } : {}),
    ...(message.forum_title ? { forumTitle: message.forum_title } : {}),
    ...(message.forum_thread_title ? { forumThreadTitle: message.forum_thread_title } : {}),
    ...(mentionUris.length ? { mentions: mentionUris.map((uri) => uri.replace(/^xmpp:/, "")) } : {}),
    ...(markupSpans.length
      ? { markup: markupSpans.flatMap((s) => { const m = wasmSpanToMarkupSpan(s); return m ? [m] : []; }) }
      : {}),
    ...(references.length ? { references: references.map(referenceFromWasm) } : {}),
    ...(associatedSharedFiles.length ? { sharedFiles: associatedSharedFiles } : {}),
    ...(associatedLinkPreviews.length ? { linkPreviews: associatedLinkPreviews } : {}),
    ...(extensionAnnotations.length ? { extensionAnnotations } : {}),
    ...(message.extension_body_fallback && extensionAnnotations.length ? { extensionBodyFallback: true } : {}),
    ...(message.is_sticker ? { isSticker: true } : {}),
    ...(message.is_retracted ? { isRetracted: true } : {}),
    ...(message.retraction_id ? { retractionId: message.retraction_id } : {}),
    ...(message.origin_id ?? message.id ? { correctionTargetId: (message.origin_id ?? message.id) as string } : {}),
    ...(message.origin_id || message.id ? { replyableId: message.origin_id ?? message.id ?? undefined } : {}),
    // XEP-0490 needs the user-server-injected stanza-id for DM
    // displayed-sync. `message.stanza_id` plus its `by` JID round-
    // trip from the wasm parser.
    ...(ownStanzaId ? { stanzaId: ownStanzaId.id, stanzaIdBy: ownStanzaId.by } : {}),
    ...(message.displayed_marker_requested ? { displayedMarkerRequested: true } : {}),
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
  if (opts.linkPreviewToken?.trim()) {
    wasmOpts.link_preview_token = opts.linkPreviewToken.trim();
  }
  if (opts.requestDisplayedMarker) {
    wasmOpts.request_displayed_marker = true;
  }
  if ((opts as SendDirectMessageOptions).mucPm) {
    wasmOpts.muc_pm = true;
  }
  if (opts.files?.length) {
    wasmOpts.shared_files = opts.files.map((file) => ({
      url: file.url,
      name: file.name,
      media_type: file.mediaType,
      size: file.size,
      width: file.width,
      height: file.height,
      // XEP-0448 requires the plaintext content hash inside <file/> for
      // encrypted sends; the Rust builder refuses encrypted files without one.
      ...(file.hashes?.length
        ? { hashes: file.hashes.map((hash) => ({ algo: hash.algo, value_b64: hash.valueB64 })) }
        : {}),
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

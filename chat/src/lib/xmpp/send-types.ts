import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { EncryptedFileHash, WaddleEncryptedFile } from "./extensions/encrypted-file";

export interface OutboundFileAttachment {
  url: string;
  name: string;
  mediaType: string;
  size: number;
  disposition: "inline" | "attachment";
  width?: number;
  height?: number;
  /**
   * XEP-0300 plaintext content hashes for the `<file/>` metadata.
   * XEP-0448 requires at least one on encrypted sends.
   */
  hashes?: EncryptedFileHash[];
  encrypted?: WaddleEncryptedFile;
}

export interface ReplyTarget {
  id: string;
  author: string;
  body?: string;
}

interface WaddleThreadCreate {
  title: string;
}

interface WaddleThreadReply {
  threadId: string;
}

export interface SendGroupMessageOptions {
  markup?: MarkupSpan[];
  references?: MessageReference[];
  mentionJidsByNick?: Readonly<Record<string, string>>;
  files?: OutboundFileAttachment[];
  linkPreviewToken?: string;
  linkPreviewExpiresAt?: string;
  replyTo?: ReplyTarget;
  threadId?: string;
  parentThreadId?: string;
  id?: string;
  requestDisplayedMarker?: boolean;
  threadCreate?: WaddleThreadCreate;
  threadReply?: WaddleThreadReply;
}

export interface SendDirectMessageOptions {
  markup?: MarkupSpan[];
  references?: MessageReference[];
  files?: OutboundFileAttachment[];
  linkPreviewToken?: string;
  linkPreviewExpiresAt?: string;
  replyTo?: ReplyTarget;
  threadId?: string;
  parentThreadId?: string;
  id?: string;
  requestDisplayedMarker?: boolean;
  /**
   * XEP-0045 §7.5: this send addresses a MUC occupant JID. The wasm
   * builder appends `<x xmlns='http://jabber.org/protocol/muc#user'/>`
   * so the sender's other clients can classify the sent-carbon copy.
   */
  mucPm?: boolean;
}

export function buildReplyFallbackPrefix(parentBody: string | undefined): { prefix: string; length: number } {
  if (!parentBody) return { prefix: "", length: 0 };
  const lines = parentBody.split("\n");
  const quoted = lines.map((line) => `> ${line}`).join("\n");
  const prefix = `${quoted}\n\n`;
  return { prefix, length: [...prefix].length };
}

export function shiftMarkupSpans<T extends { start: number; end: number }>(spans: readonly T[], offset: number): T[] {
  if (!Number.isFinite(offset)) return [];
  return spans.flatMap((span) => {
    if (!Number.isFinite(span.start) || !Number.isFinite(span.end) || span.start < 0 || span.end <= span.start) {
      return [];
    }
    const shifted = { ...span, start: span.start + offset, end: span.end + offset };
    return shifted.end > shifted.start ? [shifted] : [];
  });
}

// XEP-0372 references use `begin`/`end` (not `start`/`end`). When an outbound
// message gets a reply-fallback prefix prepended, every reference offset must
// shift right by `prefix.length`, otherwise begin/end points at the wrong
// span on the wire.
//
// Anchor-only references (no body position) are represented by the (0, 0)
// sentinel — they don't point at any body substring and MUST be preserved
// verbatim through the rebase, not shifted or dropped.
export function shiftReferenceOffsets<T extends { begin?: number; end?: number }>(
  references: readonly T[],
  offset: number,
): T[] {
  if (!Number.isFinite(offset)) return [];
  return references.flatMap((reference) => {
    if (typeof reference.begin !== "number" || typeof reference.end !== "number") return [];
    if (!Number.isFinite(reference.begin) || !Number.isFinite(reference.end)) return [];
    if (reference.begin === 0 && reference.end === 0) return [reference];
    if (reference.begin < 0 || reference.end <= reference.begin) return [];
    const shifted = { ...reference, begin: reference.begin + offset, end: reference.end + offset };
    return shifted.end > shifted.begin ? [shifted] : [];
  });
}

import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";

export type EncryptedFileCipher =
  | "urn:xmpp:ciphers:aes-128-gcm-nopadding:0"
  | "urn:xmpp:ciphers:aes-256-gcm-nopadding:0";

export interface EncryptedFileHash {
  algo: string;
  valueB64: string;
}

export interface WaddleEncryptedFile {
  cipher: EncryptedFileCipher;
  keyB64: string;
  ivB64: string;
  hashes?: EncryptedFileHash[];
  sources?: string[];
}

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
  id: string;
  author: string;
  body?: string;
}

export interface WaddleThreadCreate {
  title: string;
}

export interface WaddleThreadReply {
  threadId: string;
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

export interface SendDirectMessageOptions {
  markup?: MarkupSpan[];
  references?: MessageReference[];
  files?: OutboundFileAttachment[];
  replyTo?: ReplyTarget;
  threadId?: string;
  parentThreadId?: string;
  id?: string;
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

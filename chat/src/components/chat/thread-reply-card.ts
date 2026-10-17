import { isImageFile, isImageUrl, type TimelineMessage } from "@/lib/chat-ui";
import { formatMePreview } from "@/lib/me-command";
import type { MessageThreadEntry } from "@/channels/threads";

// The attached reply card under a post with replies: a summary line
// ("N replies · time") and, when a reply is loaded, a "author: snippet"
// line. Client-side only; derived from the loaded thread replies.

/** Longest snippet kept; the card ellipsises visually well before this. */
const SNIPPET_MAX_LENGTH = 140;

export interface ThreadLatestReply {
  /** Display name of the reply's author (the row's nick). */
  author: string;
  /** Plain one-line text; empty only for a bodyless, fileless reply. */
  snippet: string;
  /** ISO timestamp of the reply. */
  createdAt: string;
}

export function threadReplyCountLabel(count: number): string {
  return `${count} ${count === 1 ? "reply" : "replies"}`;
}

/**
 * One-line plain-text preview of a reply. Bodies are already plain text
 * (XEP-0394 markup travels as offsets beside the body), so this only
 * collapses whitespace. A body that merely repeats an attachment URL is
 * not text. Attachment-only replies say "Photo" or "Attachment";
 * retracted replies say "Message deleted".
 */
export function threadReplySnippet(message: TimelineMessage): string {
  if (message.isRetracted) return "Message deleted";
  const files = message.sharedFiles ?? [];
  const rawBody = message.body?.trim() ?? "";
  const bodyIsMedia = rawBody.length > 0 && (
    files.length === 0
      ? isImageUrl(rawBody)
      : files.some((file) => file.url === rawBody)
  );
  const hasImage = files.some((file) => isImageFile(file.mediaType, file.url))
    || (files.length === 0 && bodyIsMedia);
  // A sticker's body is alt text and a media URL body is the media itself.
  const bodyIsText = !bodyIsMedia && !(message.isSticker && hasImage);
  const text = bodyIsText ? formatMePreview(rawBody, message.author).replace(/\s+/g, " ").trim() : "";
  if (text) {
    return text.length > SNIPPET_MAX_LENGTH ? `${text.slice(0, SNIPPET_MAX_LENGTH - 1).trimEnd()}…` : text;
  }
  if (hasImage) return "Photo";
  if (files.length > 0) return "Attachment";
  return "";
}

/** The newest loaded direct reply of a thread, or `null` when none is loaded. */
export function latestThreadReply(entry: MessageThreadEntry | null | undefined): ThreadLatestReply | null {
  const latest = entry?.directChildren.at(-1);
  if (!latest) return null;
  return {
    author: latest.author,
    snippet: threadReplySnippet(latest),
    createdAt: latest.createdAt,
  };
}

/**
 * Accessible name for the card, which is a single button:
 * "N replies, latest from <author>: <snippet>, <time>. Open thread".
 */
export function threadReplyCardLabel(input: {
  count: number;
  latest: ThreadLatestReply | null;
  recency: string;
}): string {
  const parts = [threadReplyCountLabel(input.count)];
  if (input.latest) {
    parts.push(
      input.latest.snippet
        ? `latest from ${input.latest.author}: ${input.latest.snippet}`
        : `latest from ${input.latest.author}`,
    );
  }
  if (input.recency) parts.push(input.recency);
  return `${parts.join(", ")}. Open thread`;
}

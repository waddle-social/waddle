import type { WaddleEncryptedFile } from "@/lib/xmpp/extensions/encrypted-file";
import {
  renderRichMessageHtml,
  type MarkupSpan,
  type MessageReference,
} from "@/lib/rich-message";

export type { MarkupSpan, MessageReference } from "@/lib/rich-message";

export type AppState = "loading" | "signed-out" | "ready" | "error";
export type AdminTab = "rooms" | "people" | "settings";
export type EditableRole = "member" | "admin" | "owner" | "outcast";

/** Delivery status for messages sent by the current user. */
export type DeliveryStatus = "queued" | "sending" | "delivered" | "failed";

export interface TimelineSharedFile {
  name?: string;
  mediaType?: string;
  size?: number;
  width?: number;
  height?: number;
  desc?: string;
  url: string;
  disposition: "inline" | "attachment";
  encrypted?: WaddleEncryptedFile;
}

export interface TimelineMessage {
  id: string;
  /** Equivalent wire-level ids (XEP-0359 stanza/origin ids, echoed ids). */
  wireIds?: string[];
  author: string;
  /** Actual JID / occupant JID for the author when known. */
  authorJid?: string;
  body: string;
  createdAt: string;
  isSelf: boolean;
  /** Delivery status — only meaningful for self-sent messages. */
  deliveryStatus?: DeliveryStatus;
  /** Whether this message has been edited (XEP-0308). */
  isEdited?: boolean;
  /** Whether this message has been retracted (XEP-0424). */
  isRetracted?: boolean;
  /** Aggregated emoji reactions: emoji -> list of nicks (XEP-0444). */
  reactions?: Record<string, string[]>;
  /** Users who have seen this message (XEP-0333). */
  readBy?: string[];
  /** Mentioned JIDs/nicks in this message (XEP-0372). */
  mentions?: string[];
  /** XEP-0317: Hat badges for the message author. */
  hats?: { title: string; uri: string }[];
  /** XEP-0449: Is this a sticker message? */
  isSticker?: boolean;
  /** XEP-0446/0447: Shared files (zero or more attachments). */
  sharedFiles?: TimelineSharedFile[];
  /** XEP-0513: Broadcast mention (everyone/here). */
  broadcastMention?: "everyone" | "here";
  /** XEP-0394: Message Markup offset-based annotations. */
  markup?: MarkupSpan[];
  /** XEP-0372: References for links and mentions. */
  references?: MessageReference[];
  /** XEP-0461: Parent message this replies to, with optional preview text. */
  replyTo?: { id: string; author?: string; preview?: string };
  /** RFC 6121 / XEP-0201: Thread identifier + optional parent thread. */
  threadId?: string;
  parentThreadId?: string;
  /** XEP-0508: Forum topic/reply metadata when present. */
  forumPostKind?: "topic" | "reply";
  forumTitle?: string;
  forumThreadTitle?: string;
}

export interface CommunityFormData {
  name: string;
  description: string;
  is_public: boolean;
}

export interface ChannelCreateFormData {
  name: string;
  description: string;
  channel_type: string;
  position: number;
}

export interface ChannelEditFormData {
  name: string;
  description: string;
  position: number;
}

// ── XEP-0392 Consistent Color Generation ────────────────────────────

/** Compute a deterministic hue (0–360) from a string using XEP-0392 algorithm. */
function consistentHue(input: string): number {
  // Simple SHA-1-like hash using SubtleCrypto isn't available synchronously.
  // Use the same algorithm: hash → first 2 bytes → hue mapping.
  // We use a simple DJB2-variant that gives good distribution for short strings.
  let h = 5381;
  for (let i = 0; i < input.length; i++) {
    h = ((h << 5) + h + input.charCodeAt(i)) & 0xffff;
  }
  return (h / 65536) * 360;
}

/** Generate a CSS hsl() color string for a username/JID. */
export function consistentColor(input: string, saturation = 65, lightness = 50): string {
  const hue = consistentHue(input);
  return `hsl(${Math.round(hue)}, ${saturation}%, ${lightness}%)`;
}

// ── Rich Message Rendering ──────────────────────────────────────────

export function renderStyledBody(
  body: string,
  markup?: readonly MarkupSpan[],
  references?: readonly MessageReference[],
): string {
  return renderRichMessageHtml({ body, markup, references });
}

// ── Image / GIF URL detection ────────────────────────────────────────

const IMAGE_URL_RE = /^https?:\/\/\S+\.(?:gif|png|jpe?g|webp|avif|bmp|svg)(?:[?#]\S*)?$/i;
const VIDEO_URL_RE = /^https?:\/\/\S+\.(?:mp4|webm|mov|m4v|ogv)(?:[?#]\S*)?$/i;
const AUDIO_URL_RE = /^https?:\/\/\S+\.(?:mp3|wav|ogg|oga|m4a|aac|flac)(?:[?#]\S*)?$/i;
const PDF_URL_RE = /^https?:\/\/\S+\.pdf(?:[?#]\S*)?$/i;
const IMAGE_EXTENSION_RE = /\.(?:gif|png|jpe?g|webp|avif|bmp|svg)(?:[?#]\S*)?$/i;
const VIDEO_EXTENSION_RE = /\.(?:mp4|webm|mov|m4v|ogv)(?:[?#]\S*)?$/i;
const AUDIO_EXTENSION_RE = /\.(?:mp3|wav|ogg|oga|m4a|aac|flac)(?:[?#]\S*)?$/i;
const PDF_EXTENSION_RE = /\.pdf(?:[?#]\S*)?$/i;
const GIPHY_URL_RE = /^https?:\/\/(?:media\d*\.giphy\.com|i\.giphy\.com)\//i;
const IMAGE_MEDIA_TYPE_RE = /^image\//i;
const VIDEO_MEDIA_TYPE_RE = /^video\//i;
const AUDIO_MEDIA_TYPE_RE = /^audio\//i;

/** Check if a message body is a single image/GIF URL that should render inline. */
export function isImageUrl(body: string): boolean {
  const trimmed = body.trim();
  return IMAGE_URL_RE.test(trimmed) || GIPHY_URL_RE.test(trimmed);
}

/** Check if an attachment should render as an inline image/GIF preview. */
export function isImageFile(mediaType?: string, url?: string): boolean {
  if (mediaType && IMAGE_MEDIA_TYPE_RE.test(mediaType)) return true;
  if (!url) return false;
  const candidate = url.trim();
  return isImageUrl(candidate) || IMAGE_EXTENSION_RE.test(candidate);
}

export function isVideoFile(mediaType?: string, url?: string): boolean {
  if (mediaType && VIDEO_MEDIA_TYPE_RE.test(mediaType)) return true;
  if (!url) return false;
  return VIDEO_URL_RE.test(url.trim()) || VIDEO_EXTENSION_RE.test(url.trim());
}

export function isAudioFile(mediaType?: string, url?: string): boolean {
  if (mediaType && AUDIO_MEDIA_TYPE_RE.test(mediaType)) return true;
  if (!url) return false;
  return AUDIO_URL_RE.test(url.trim()) || AUDIO_EXTENSION_RE.test(url.trim());
}

export function isPdfFile(mediaType?: string, url?: string): boolean {
  if (mediaType === "application/pdf") return true;
  if (!url) return false;
  return PDF_URL_RE.test(url.trim()) || PDF_EXTENSION_RE.test(url.trim());
}

export function inferredFileDisposition(
  mediaType?: string,
  url?: string,
): "inline" | "attachment" {
  return isImageFile(mediaType, url)
    || isVideoFile(mediaType, url)
    || isAudioFile(mediaType, url)
    || isPdfFile(mediaType, url)
    ? "inline"
    : "attachment";
}

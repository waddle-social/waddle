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

export type GitHubEmbedKind = "repo" | "issue" | "pr";

export interface GitHubEmbed {
  kind: GitHubEmbedKind;
  url: string;
  owner: string;
  name: string;
}

export interface TimelineMessage {
  id: string;
  /** Equivalent wire-level ids (XEP-0359 stanza/origin ids, echoed ids). */
  wireIds?: string[];
  /** XEP-0308 correction target: original sender message id/origin-id. */
  correctionTargetId?: string;
  author: string;
  /** Actual bare JID when known; otherwise the room occupant JID. */
  authorJid?: string;
  /** Full MUC occupant JID used for same-occupant protocol checks. */
  authorOccupantJid?: string;
  /** XEP-0313 MUC archive real JID from muc#user item@jid, when present. */
  authorRealJid?: string;
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
  /** Waddle GitHub enrichment embeds. */
  githubEmbeds?: GitHubEmbed[];
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
  /** Waddle thread metadata when present. */
  forumPostKind?: "topic" | "reply";
  forumTitle?: string;
  forumThreadTitle?: string;
}

export interface CommunityFormData {
  name: string;
  description: string;
  is_public: boolean;
}

// ── Create intent typed form model ──────────────────────────────────

/** The four possible targets for the create flow. */
export type CreateIntent = "space" | "muc" | "space-muc" | "space-with-muc";

/** MUC subtype — only applicable when a MUC is being created. */
export type MucSubtype = "text" | "forum";

/** Create a new Space (empty — do not auto-create a default MUC). */
export interface CreateSpaceFormData {
  intent: "space";
  name: string;
  description: string;
}

/** Create a standalone MUC (not attached to any Space). */
export interface CreateMucFormData {
  intent: "muc";
  name: string;
  description: string;
  muc_type: MucSubtype;
}

/** Create a MUC inside an existing Space. */
export interface CreateSpaceMucFormData {
  intent: "space-muc";
  /** PubSub node id of the target Space (not a bare JID). */
  space_node: string;
  name: string;
  description: string;
  muc_type: MucSubtype;
}

/** Create a new Space together with its first MUC in one action. */
export interface CreateSpaceWithMucFormData {
  intent: "space-with-muc";
  space_name: string;
  space_description: string;
  muc_name: string;
  muc_description: string;
  muc_type: MucSubtype;
}

export type CreateFormData =
  | CreateSpaceFormData
  | CreateMucFormData
  | CreateSpaceMucFormData
  | CreateSpaceWithMucFormData;

/** Returns a freshly initialised default create form (standalone MUC). */
export function defaultCreateForm(): CreateMucFormData {
  return { intent: "muc", name: "", description: "", muc_type: "text" };
}

// ── Create result typed model ────────────────────────────────────────

/** Returned by createChannel() for a Space-only creation — no room was created. */
export interface CreateSpaceResult {
  intent: "space";
  spaceId: string;
  spaceName: string;
}

/** Returned by createChannel() when a MUC was created (standalone or in/with a Space). */
export interface CreateMucResult {
  intent: "muc" | "space-muc" | "space-with-muc";
  channelId: string;
  channelJid: string;
  channelName: string;
  channelType: MucSubtype;
  /** Present when a Space was involved (space-muc or space-with-muc). */
  spaceNode?: string;
}

/** Discriminated union of all possible create results. */
export type CreateChannelResult = CreateSpaceResult | CreateMucResult;

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

export function githubEmbedKindLabel(kind: GitHubEmbedKind): string {
  switch (kind) {
    case "issue":
      return "Issue";
    case "pr":
      return "Pull request";
    default:
      return "Repository";
  }
}

export function githubEmbedNumber(embed: GitHubEmbed): string | null {
  const marker = embed.kind === "issue" ? "/issues/" : embed.kind === "pr" ? "/pull/" : "";
  if (!marker) return null;
  try {
    const url = new URL(embed.url);
    const value = url.pathname.split(marker)[1]?.split("/")[0];
    return value && /^\d+$/.test(value) ? value : null;
  } catch {
    const value = embed.url.split(marker)[1]?.split(/[/?#]/)[0];
    return value && /^\d+$/.test(value) ? value : null;
  }
}

export function githubEmbedDisplayTitle(embed: GitHubEmbed): string {
  const repo = `${embed.owner}/${embed.name}`;
  const number = githubEmbedNumber(embed);
  return number ? `${repo} #${number}` : repo;
}

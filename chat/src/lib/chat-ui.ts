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

export type ExtensionSurfaceKind =
  | "message-card"
  | "board"
  | "game"
  | "chat-bot"
  | "dynamic-canvas"
  | "utility-panel";

export interface ExtensionEnvelopeSource {
  stanzaId: string;
  by?: string;
  bodyStart?: number;
  bodyEnd?: number;
}

export interface ExtensionPayloadElement {
  namespace: string;
  name: string;
  attributes: Record<string, string>;
  text?: string;
  children: ExtensionPayloadElement[];
}

export type ExtensionUiBlockKind = "text" | "image" | "action" | "form" | "list" | "unknown";

export interface ExtensionUiBlock {
  kind: ExtensionUiBlockKind;
  id?: string;
  title?: string;
  text?: string;
  label?: string;
  description?: string;
  style?: string;
  launchId?: string;
  attributes: Record<string, string>;
  children: ExtensionUiBlock[];
}

export interface ExtensionUiView {
  id: string;
  title?: string;
  blocks: ExtensionUiBlock[];
}

export interface ExtensionLaunchContext {
  waddleId?: string;
  roomJid?: string;
  stanzaId?: string;
}

export interface ExtensionLaunchDescriptor {
  id: string;
  pluginId: string;
  actionId: string;
  commandNode: string;
  launchToken?: string;
  label: string;
  context: ExtensionLaunchContext;
  expiresAt?: string;
  source?: ExtensionEnvelopeSource;
  payloads: ExtensionPayloadElement[];
}

export interface ExtensionAnnotationAction {
  label: string;
  route: string;
  launch?: ExtensionLaunchDescriptor;
}

export interface ExtensionAnnotation {
  extensionId: string;
  annotationId: string;
  surfaceKind: ExtensionSurfaceKind;
  title: string;
  summary?: string;
  imageUrl?: string;
  source?: ExtensionEnvelopeSource;
  payloadNamespace?: string;
  views?: ExtensionUiView[];
  payloads?: ExtensionPayloadElement[];
  fields: Record<string, string>;
  actions: ExtensionAnnotationAction[];
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
  /** XEP-0444 groupchat reaction target: room-assigned XEP-0359 stanza-id. */
  reactionTargetId?: string;
  /**
   * XEP-0461 §3.2 replyable id. Absent on groupchat messages that lack a
   * room-assigned XEP-0359 stanza-id; consumers MUST treat such messages as
   * non-replyable (disable the action, refuse to send) rather than fall back
   * to the wire id.
   */
  replyableId?: string;
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
  /** Archived reaction sender identities: emoji -> sender id -> display nick. */
  reactionSenders?: Record<string, Record<string, string>>;
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
  /** Waddle unified extension framework annotations. */
  extensionAnnotations?: ExtensionAnnotation[];
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

/** Returns the best create form for the current navigation container. */
export function defaultCreateFormForContext(spaceNode?: string | null): CreateMucFormData | CreateSpaceMucFormData {
  if (spaceNode) {
    return {
      intent: "space-muc",
      space_node: spaceNode,
      name: "",
      description: "",
      muc_type: "text",
    };
  }

  return defaultCreateForm();
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

export function extensionSurfaceLabel(kind: ExtensionSurfaceKind): string {
  switch (kind) {
    case "board":
      return "Board";
    case "game":
      return "Game";
    case "chat-bot":
      return "Chat bot";
    case "dynamic-canvas":
      return "Dynamic canvas";
    case "utility-panel":
      return "Utility";
    default:
      return "Extension";
  }
}

function humanizeExtensionKey(value: string): string {
  return value
    .replace(/^payload#/, "")
    .replace(/^waddle#/, "waddle ")
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[-_:#]+/g, " ")
    .trim()
    .replace(/\s+/g, " ")
    .replace(/\b\w/g, (char) => char.toUpperCase());
}

function payloadText(element: ExtensionPayloadElement): string | null {
  if (element.text?.trim()) return element.text.trim();
  for (const child of element.children) {
    const text = payloadText(child);
    if (text) return text;
  }
  return null;
}

interface ExtensionCardDetail {
  label: string;
  value: string;
}

export function extensionCardDetails(annotation: ExtensionAnnotation, limit = 6): ExtensionCardDetail[] {
  const details: ExtensionCardDetail[] = [];
  const seen = new Set<string>();

  const add = (label: string, value: string | undefined) => {
    const trimmed = value?.trim();
    if (!trimmed) return;
    const key = `${label}:${trimmed}`;
    if (seen.has(key)) return;
    seen.add(key);
    details.push({ label, value: trimmed });
  };

  for (const [key, value] of Object.entries(annotation.fields)) {
    if (key === "surface" || key === "payloadNamespace") continue;
    add(humanizeExtensionKey(key), value);
  }

  const payload = annotation.payloads?.[0];
  if (payload) {
    for (const [key, value] of Object.entries(payload.attributes)) {
      if (key === "xmlns") continue;
      add(humanizeExtensionKey(key), value);
      if (details.length >= limit) return details.slice(0, limit);
    }
    for (const child of payload.children) {
      const text = payloadText(child);
      if (text) add(humanizeExtensionKey(child.name), text);
      if (details.length >= limit) return details.slice(0, limit);
    }
  }

  return details.slice(0, limit);
}

type ExtensionPresentationKind =
  | "links-task-board"
  | "pub-quiz"
  | "decision-polls"
  | "generic";

interface ExtensionPresentationOption {
  id: string;
  label: string;
  value?: number;
}

interface ExtensionPresentation {
  kind: ExtensionPresentationKind;
  label: string;
  title: string;
  summary?: string;
  primaryValue?: string;
  secondaryValue?: string;
  options: ExtensionPresentationOption[];
  details: ExtensionCardDetail[];
}

export function extensionPresentation(annotation: ExtensionAnnotation): ExtensionPresentation {
  const payload = annotation.payloads?.[0];
  const plugin = annotation.extensionId;
  if (plugin === "decision-polls" || payload?.namespace === "urn:waddle:decision-polls:1") {
    return decisionPollPresentation(annotation, payload);
  }
  if (plugin === "pub-quiz" || payload?.namespace === "urn:waddle:pub-quiz:1") {
    return pubQuizPresentation(annotation, payload);
  }
  if (plugin === "links-task-board" || payload?.namespace === "urn:waddle:links-task-board:1") {
    return linksTaskBoardPresentation(annotation, payload);
  }
  const summary = genericExtensionSummary(annotation, payload);
  return {
    kind: "generic",
    label: extensionSurfaceLabel(annotation.surfaceKind),
    title: annotation.title,
    ...(summary ? { summary } : {}),
    options: [],
    details: extensionCardDetails(annotation),
  };
}

export function extensionActionStatusLabel(state?: "loading" | "success" | "warning" | "error"): string {
  switch (state) {
    case "loading":
      return "Working";
    case "success":
      return "Done";
    case "warning":
      return "Needs attention";
    case "error":
      return "Failed";
    default:
      return "";
  }
}

function childText(payload: ExtensionPayloadElement | undefined, name: string): string | undefined {
  const text = payload?.children.find((child) => child.name === name)?.text?.trim();
  return text || undefined;
}

function payloadAttr(payload: ExtensionPayloadElement | undefined, ...names: string[]): string | undefined {
  for (const name of names) {
    const value = payload?.attributes[name]?.trim();
    if (value) return value;
  }
  return undefined;
}

function genericExtensionSummary(
  annotation: ExtensionAnnotation,
  payload: ExtensionPayloadElement | undefined,
): string | undefined {
  const summary = annotation.summary?.trim();
  if (summary && summary !== annotation.payloadNamespace) return summary;
  const text = payload ? payloadText(payload) : null;
  if (text && text !== annotation.title) return text;
  return undefined;
}

function payloadOptions(payload: ExtensionPayloadElement | undefined): ExtensionPresentationOption[] {
  return payload?.children
    .filter((child) => child.name === "option" || child.name === "answer")
    .map((child, index) => ({
      id: child.attributes.id ?? child.attributes["option-id"] ?? String(index),
      label: child.text?.trim() || child.attributes.label || child.attributes.title || `Option ${index + 1}`,
      ...(Number.isFinite(Number(child.attributes.votes)) ? { value: Number(child.attributes.votes) } : {}),
    })) ?? [];
}

function decisionPollPresentation(
  annotation: ExtensionAnnotation,
  payload: ExtensionPayloadElement | undefined,
): ExtensionPresentation {
  const question = childText(payload, "question") ?? payloadAttr(payload, "question") ?? annotation.title;
  const status = payloadAttr(payload, "status", "state") ?? payloadAttr(payload, "closes-at");
  return {
    kind: "decision-polls",
    label: "Poll",
    title: question,
    ...(status ? { summary: status } : annotation.summary ? { summary: annotation.summary } : {}),
    options: payloadOptions(payload),
    details: extensionCardDetails(annotation, 3),
  };
}

function pubQuizPresentation(
  annotation: ExtensionAnnotation,
  payload: ExtensionPayloadElement | undefined,
): ExtensionPresentation {
  const question = childText(payload, "question") ?? payloadAttr(payload, "question") ?? annotation.title;
  const round = payloadAttr(payload, "round", "round-id");
  return {
    kind: "pub-quiz",
    label: "Quiz",
    title: question,
    ...(round ? { summary: `Round ${round}` } : annotation.summary ? { summary: annotation.summary } : {}),
    options: payloadOptions(payload),
    details: extensionCardDetails(annotation, 3),
  };
}

function linksTaskBoardPresentation(
  annotation: ExtensionAnnotation,
  payload: ExtensionPayloadElement | undefined,
): ExtensionPresentation {
  const title = payloadAttr(payload, "title", "name") ?? childText(payload, "title") ?? annotation.title;
  const url = payloadAttr(payload, "url", "href");
  const site = payloadAttr(payload, "site", "host");
  return {
    kind: "links-task-board",
    label: payload?.name === "task" ? "Task" : "Link",
    title,
    ...(site || url ? { summary: site ?? url } : annotation.summary ? { summary: annotation.summary } : {}),
    ...(url ? { primaryValue: url } : {}),
    options: [],
    details: extensionCardDetails(annotation, 4),
  };
}

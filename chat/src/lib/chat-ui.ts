import type { PinPermission } from "@/lib/chat-types";
import type { TimestampSource } from "@/lib/timeline-timestamps";
import type { WaddleEncryptedFile } from "@/lib/xmpp/extensions/encrypted-file";
import {
  renderRichMessageHtml,
  type MarkupSpan,
  type MessageReference,
} from "@/lib/rich-message";

export type { MarkupSpan, MessageReference } from "@/lib/rich-message";
export {
  inferredFileDisposition,
  isAudioFile,
  isImageFile,
  isImageUrl,
  isPdfFile,
  isVideoFile,
} from "@/lib/message-media";

export type AppState = "loading" | "signed-out" | "ready" | "error";
export type AdminTab = "rooms" | "people" | "settings";
/** The subset of XEP-0045 affiliations the member-management UI lets
 * an admin assign. Previously named `EditableRole`; XMPP `role`
 * means moderator/participant/visitor/none (session-scoped), which
 * isn't what this picker selects — it picks the persistent
 * affiliation. */
export type EditableAffiliation = "member" | "admin" | "owner" | "outcast";

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

export interface CallThreadAnchor {
  kind: "muc";
  /**
   * Set on anchors derived from timeline messages (the inbound
   * `urn:waddle:call-thread:0` marker). Absent on anchors derived from a
   * global Threads-query entry, which only carries kind/media/ended/duration —
   * `readCallAnchorCardState` and the card never read these.
   */
  sid?: string;
  media: readonly ("audio" | "video")[];
  initiator?: string;
  started?: string;
  ended?: string;
  duration?: string;
}

export interface LinkPreview {
  originalUrl: string;
  normalizedUrl?: string;
  title?: string;
  description?: string;
  image?: LinkPreviewImage;
  video?: LinkPreviewVideo;
  playerEmbed?: LinkPreviewPlayer;
  remoteMediaUnavailable?: boolean;
}

interface LinkPreviewImage {
  url: string;
  mediaType: string;
  width?: number;
  height?: number;
  alt?: string;
}

/// Trusted direct playable video the client may inline-play on user action.
export interface LinkPreviewVideo {
  url: string;
  mediaType: string;
  size?: number;
}

/** Embeddable player iframe (allowlisted og:video) the client renders on click. */
export interface LinkPreviewPlayer {
  url: string;
  width?: number;
  height?: number;
}

export type LinkPreviewMediaState =
  | { kind: "none" }
  | { kind: "image"; image: LinkPreviewImage }
  | { kind: "video"; video: LinkPreviewVideo; poster?: LinkPreviewImage }
  | { kind: "player"; player: LinkPreviewPlayer; poster?: LinkPreviewImage }
  | { kind: "remote-unavailable" };

export function linkPreviewMediaState(preview: LinkPreview): LinkPreviewMediaState {
  if (preview.video) {
    return { kind: "video", video: preview.video, ...(preview.image ? { poster: preview.image } : {}) };
  }
  if (preview.playerEmbed) {
    return { kind: "player", player: preview.playerEmbed, ...(preview.image ? { poster: preview.image } : {}) };
  }
  if (preview.image) return { kind: "image", image: preview.image };
  if (preview.remoteMediaUnavailable) return { kind: "remote-unavailable" };
  return { kind: "none" };
}

export type ExtensionSurfaceKind =
  | "message-card"
  | "board"
  | "game"
  | "chat-bot"
  | "dynamic-canvas"
  | "utility-panel";

interface ExtensionEnvelopeSource {
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

type ExtensionUiBlockKind = "text" | "image" | "action" | "form" | "list" | "unknown";

interface ExtensionUiBlock {
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

interface ExtensionUiView {
  id: string;
  title?: string;
  blocks: ExtensionUiBlock[];
}

interface ExtensionLaunchContext {
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
  /** XEP-0359 stanza-id of this message (room-injected for MUC,
   *  user-server-injected for DM). Used by XEP-0490 MDS publish. */
  stanzaId?: string;
  /** Bare JID that injected `stanzaId` (room for MUC, server for DM). */
  stanzaIdBy?: string;
  body: string;
  createdAt: string;
  /**
   * Provenance of `createdAt`. Merges across producers prefer the
   * higher-authority source so a redelivered live stanza without a
   * `<delay/>` (decoded as `fallback`) cannot overwrite a true
   * server stamp (`archive`/`delay`) that landed via MAM first.
   */
  createdAtSource: TimestampSource;
  isSelf: boolean;
  /** Delivery status — only meaningful for self-sent messages. */
  deliveryStatus?: DeliveryStatus;
  /** Whether this message has been edited (XEP-0308). */
  isEdited?: boolean;
  /** Whether this message has been retracted (XEP-0424). */
  isRetracted?: boolean;
  /** XEP-0424 retraction message id preserved from tombstone archives. */
  retractionId?: string;
  /** Aggregated emoji reactions: emoji -> list of nicks (XEP-0444). */
  reactions?: Record<string, string[]>;
  /** Archived reaction sender identities: emoji -> sender id -> display nick. */
  reactionSenders?: Record<string, Record<string, string>>;
  /** Users who have seen this message (XEP-0333). */
  readBy?: string[];
  /** Whether this received message requested XEP-0333 displayed markers. */
  displayedMarkerRequested?: boolean;
  /** Mentioned JIDs/nicks in this message (XEP-0372). */
  mentions?: string[];
  /** XEP-0317: Hat badges for the message author. */
  hats?: { title: string; uri: string }[];
  /** XEP-0449: Is this a sticker message? */
  isSticker?: boolean;
  /** XEP-0446/0447: Shared files (zero or more attachments). */
  sharedFiles?: TimelineSharedFile[];
  /** XEP-0511: Link preview metadata stamped into the message payload. */
  linkPreviews?: LinkPreview[];
  /** Waddle unified extension framework annotations. */
  extensionAnnotations?: ExtensionAnnotation[];
  /** XEP-0428: message body is fallback text for the Waddle extension payload. */
  extensionBodyFallback?: boolean;
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
  /** Waddle call-thread anchor marker (`urn:waddle:call-thread:0`). */
  callThread?: CallThreadAnchor;
  /** Waddle thread metadata when present. */
  forumPostKind?: "topic" | "reply";
  forumTitle?: string;
  forumThreadTitle?: string;
  /** #414: room-authored `<pin-event/>` system message; the timeline
   * renders these distinctly (no avatar, italic). `pinEventAction`
   * carries the action when set. */
  isPinEvent?: boolean;
  pinEventAction?: "pinned" | "unpinned";
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
  /** #415: who may pin/unpin messages in this channel. `undefined`
   * means "the user did not touch the select" — the save handler
   * skips the field so the server's existing value round-trips
   * through the GET-then-SET unchanged. Once the user picks an
   * option from the dialog, this carries that explicit choice. */
  pinPermission?: PinPermission;
}

// ── XEP-0392 Consistent Color Generation ────────────────────────────
//
// The spec algorithm is SHA-1(UTF-8 input) → first 2 bytes → hue. The
// browser's SubtleCrypto SHA-1 is async, which doesn't fit Vue
// `computed` callsites (e.g. `AppAvatar`'s `bgColor`), so the chat
// loads the Rust SHA-1 impl through wasm and calls it synchronously
// once initialised. Until the wasm hue function is installed (early
// during sign-in, before `AppAvatar` typically renders), a DJB2
// fallback keeps the first paint deterministic and non-empty.

let hueImpl: (input: string) => number = djb2FallbackHue;

/** DJB2 fallback used before the wasm SHA-1 hue is installed. */
function djb2FallbackHue(input: string): number {
  let h = 5381;
  for (let i = 0; i < input.length; i++) {
    h = ((h << 5) + h + input.charCodeAt(i)) & 0xffff;
  }
  return (h / 65536) * 360;
}

/** Install the spec-conformant hue implementation. Called at app boot
 * once the wasm module is loaded; subsequent `consistentColor` calls
 * return XEP-0392 values.
 *
 * Refuses non-function inputs so a stale wasm bundle that lacks
 * `xep0392_consistent_hue` (e.g. when wasm-pack's named re-export list
 * has fallen behind the Rust source) degrades to the DJB2 fallback
 * instead of replacing `hueImpl` with `undefined` and crashing every
 * `AppAvatar` render with "T is not a function". */
export function setConsistentColorBackend(fn: (input: string) => number): void {
  if (typeof fn !== "function") return;
  hueImpl = fn;
}

/** Generate a CSS hsl() color string for a username/JID. */
export function consistentColor(input: string, saturation = 65, lightness = 50): string {
  const hue = hueImpl(input);
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
    if (isInternalExtensionDetailKey(key)) continue;
    add(humanizeExtensionKey(key), value);
  }

  const payload = annotation.payloads?.[0];
  if (payload) {
    for (const [key, value] of Object.entries(payload.attributes)) {
      if (isInternalExtensionDetailKey(key)) continue;
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

function isInternalExtensionDetailKey(key: string): boolean {
  return key === "surface"
    || key === "payloadNamespace"
    || key === "xmlns"
    || key === "poll-id"
    || key === "option-count"
    || key === "normalized-url"
    || key === "source-stanza-id"
    || key === "body-start"
    || key === "body-end"
    || key === "installation-id"
    || key === "repository-id"
    || /^option-\d+-(?:id|label)$/.test(key);
}

type ExtensionPresentationKind = "generic" | "github-event";
type ExtensionPresentationTone = "neutral" | "success" | "warning" | "danger";

/**
 * Extension intent — declares the user's relationship to the card content,
 * which drives layout independently of whether `annotation.actions` exists:
 *
 *  - `event`  → read-first. The payload IS the value. Server-pushed
 *               notifications, status updates, workflow runs. Actions
 *               (when present) render as secondary chips.
 *  - `tool`   → do-first. The action button(s) ARE why the card exists.
 *               Link Board "Save", polls/surveys, anything where the user's
 *               primary motion is to make a decision in-line.
 *
 * Intent is encoded in `extensionPresentation()` from the payload shape;
 * it never collapses to `annotation.actions.length > 0`, because an event
 * (e.g. a failing CI run) can legitimately offer affordances ("Rerun",
 * "View logs") without ceasing to be a read-first surface.
 */
type ExtensionIntent = "event" | "tool";

interface ExtensionPresentationOption {
  id: string;
  label: string;
  value?: number;
}

interface ExtensionPresentation {
  intent: ExtensionIntent;
  kind: ExtensionPresentationKind;
  tone: ExtensionPresentationTone;
  label: string;
  title: string;
  summary?: string;
  primaryValue?: string;
  secondaryValue?: string;
  primaryUrl?: string;
  options: ExtensionPresentationOption[];
  details: ExtensionCardDetail[];
}

export function extensionPresentation(annotation: ExtensionAnnotation): ExtensionPresentation {
  const payload = annotation.payloads?.[0];
  if (payload?.namespace === "urn:waddle:web-integration:1" && payload.name === "github-event") {
    return githubEventPresentation(annotation, payload);
  }
  const summary = genericExtensionSummary(annotation, payload);
  const options = payloadOptions(payload);
  const details = extensionCardDetails(annotation);
  // A generic card is a `tool` when its primary purpose is the action
  // (Link Board "Save", forms, surveys with options to pick). Without
  // actions or options, it degrades to `event` — read-first content
  // that just happens to come from an extension.
  const intent: ExtensionIntent =
    annotation.actions.length > 0 || options.length > 0 ? "tool" : "event";
  return {
    intent,
    kind: "generic",
    tone: "neutral",
    label: extensionSurfaceLabel(annotation.surfaceKind),
    title: annotation.title,
    ...(summary ? { summary } : {}),
    options,
    details,
  };
}

function githubEventPresentation(
  annotation: ExtensionAnnotation,
  payload: ExtensionPayloadElement,
): ExtensionPresentation {
  const attrs = payload.attributes;
  const repository = attrs.repository || "GitHub";
  const name = attrs.name || attrs["event-type"] || "event";
  const action = attrs.action || "received";
  const conclusion = attrs.conclusion || "";
  const summary = annotation.summary || githubSummary(repository, name, action, conclusion);
  const details: ExtensionCardDetail[] = [
    { label: "Repository", value: repository },
    ...(attrs.branch ? [{ label: "Branch", value: attrs.branch }] : []),
    ...(attrs.revision ? [{ label: "Commit", value: attrs.revision.slice(0, 7) }] : []),
    ...(attrs["event-type"] ? [{ label: "Event", value: humanizeExtensionKey(attrs["event-type"]) }] : []),
  ];
  const primaryUrl = safeExternalUrl(attrs.url);
  // GitHub events are always `event` intent — even when they carry actions
  // (rerun, approve, view logs), the user's primary motion is to read and
  // optionally click through, not to operate the card as a tool.
  return {
    intent: "event",
    kind: "github-event",
    tone: githubTone(conclusion),
    label: "GitHub",
    title: name,
    summary,
    primaryValue: conclusion && conclusion !== "unknown" ? humanizeExtensionKey(conclusion) : humanizeExtensionKey(action),
    // `secondaryValue` previously echoed the repository as a prominent
    // un-labeled meta-item — a duplicate of the Repository detail below.
    // GitHub-event cards now use a positional chip strip (labels hidden
    // via CSS for kind=github-event) so the repo appears exactly once,
    // alongside branch / commit / event. Dropping the dupe gets the
    // card from ~6 lines to ~3 on the same payload.
    ...(primaryUrl ? { primaryUrl } : {}),
    options: [],
    details,
  };
}

function safeExternalUrl(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  if (!trimmed) return undefined;
  try {
    const url = new URL(trimmed);
    return url.protocol === "http:" || url.protocol === "https:" ? trimmed : undefined;
  } catch {
    return undefined;
  }
}

function githubSummary(repository: string, name: string, action: string, conclusion: string): string {
  if (conclusion && conclusion !== "unknown") {
    return `${repository}: ${name} ${action} with ${conclusion.replace(/_/g, " ")}`;
  }
  return `${repository}: ${name} ${action}`;
}

function githubTone(conclusion: string | undefined): ExtensionPresentationTone {
  switch (conclusion) {
    case "success":
      return "success";
    case "cancelled":
    case "timed_out":
    case "failure":
    case "error":
      return "danger";
    case "unknown":
    case undefined:
    case "":
      return "neutral";
    default:
      return "warning";
  }
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
  if (!payload) return [];
  const childOptions = payload.children
    .filter((child) => child.name === "option" || child.name === "answer")
    .map((child, index) => ({
      id: child.attributes.id ?? child.attributes["option-id"] ?? String(index),
      label: child.text?.trim() || child.attributes.label || child.attributes.title || `Option ${index + 1}`,
      ...(Number.isFinite(Number(child.attributes.votes)) ? { value: Number(child.attributes.votes) } : {}),
    }));
  if (childOptions.length > 0) return childOptions;
  return attributeOptions(payload);
}

function attributeOptions(payload: ExtensionPayloadElement): ExtensionPresentationOption[] {
  const options: ExtensionPresentationOption[] = [];
  for (let index = 0; index < 50; index += 1) {
    const label = payload.attributes[`option-${index}-label`]?.trim();
    const id = payload.attributes[`option-${index}-id`]?.trim();
    if (!label && !id) break;
    options.push({
      id: id || String(index),
      label: label || `Option ${index + 1}`,
    });
  }
  return options;
}

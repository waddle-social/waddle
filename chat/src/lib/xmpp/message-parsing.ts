/** Inbound message parsing — extracts XEP extension data from stanza messages. */
import type { ReceivedMessage } from "stanza/protocol";
import type { WaddleEncryptedFile } from "./extensions/encrypted-file";
import { splitMessageIds } from "@/lib/message-ids";
import { stripMarkupRange } from "./extensions/markup";
import { codePointLength, codePointToCodeUnitIndex } from "@/lib/text-offsets";
import type {
  ExtensionAnnotation,
  ExtensionEnvelopeSource,
  ExtensionLaunchDescriptor,
  ExtensionPayloadElement,
  ExtensionSurfaceKind,
  MessageReference,
} from "@/lib/chat-ui";
import type {
  ChatStateEvent, ChatStateType, DisplayedEvent,
  LiveDmMessage, LiveRoomMessage, ReactionEvent, RoomActivityEvent, SharedFileInfo,
} from "./types";

type MessageExtensionsTarget = Pick<
  LiveRoomMessage,
  | "id"
  | "wireIds"
  | "body"
  | "mentions"
  | "references"
  | "broadcastMention"
  | "sharedFiles"
  | "extensionAnnotations"
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

type MucUserPayload = {
  jid?: unknown;
  item?: { jid?: unknown };
  items?: Array<{ jid?: unknown }>;
  users?: Array<{ jid?: unknown }>;
};

/** Access custom JXT extension fields that TypeScript doesn't know about. */
export function ext(msg: unknown): Record<string, unknown> {
  return msg as Record<string, unknown>;
}

function hasBodyOrSubject(msg: ReceivedMessage): boolean {
  return !!msg.body || !!msg.subject;
}

/**
 * True when a stanza should be treated as a user-visible message payload even
 * if `<body/>` is omitted (for example, pure file-sharing messages).
 */
export function hasRenderableMessagePayload(msg: ReceivedMessage): boolean {
  if (hasBodyOrSubject(msg)) return true;
  const stanza = ext(msg);
  const fileSharing = stanza.fileSharing;
  if (Array.isArray(fileSharing)) return fileSharing.length > 0;
  if (fileSharing) return true;
  return !!stanza.sticker || extensionAnnotationsFromStanza(stanza).length > 0;
}

interface OriginIdPayload {
  id?: string;
}

interface StanzaIdPayload {
  id?: string;
  by?: string;
}

function asArray<T>(value: T | T[] | undefined): T[] {
  if (!value) return [];
  return Array.isArray(value) ? value : [value];
}

function bareJid(value: unknown): string | undefined {
  if (typeof value !== "string" || !value.includes("@")) return undefined;
  return value.split("/")[0] || undefined;
}

function extractMucUserRealJid(msg: ReceivedMessage): string | undefined {
  const muc = (ext(msg).muc ?? ext(msg).mucUser) as MucUserPayload | undefined;
  return bareJid(muc?.jid)
    ?? bareJid(muc?.item?.jid)
    ?? bareJid(muc?.items?.find((item) => item.jid)?.jid)
    ?? bareJid(muc?.users?.find((item) => item.jid)?.jid);
}

export function resolveMessageIds(
  msg: ReceivedMessage,
  preferredStanzaBy?: string,
): { id: string; wireIds?: string[] } {
  const extMsg = ext(msg);
  const originId = extMsg.originId as OriginIdPayload | undefined;
  const stanzaIds = asArray(extMsg.stanzaIds as StanzaIdPayload | StanzaIdPayload[] | undefined);
  const preferredStanzaId = preferredStanzaBy
    ? stanzaIds.find((candidate) => candidate.by === preferredStanzaBy)?.id
    : undefined;

  return splitMessageIds(
    preferredStanzaId ?? stanzaIds[0]?.id ?? originId?.id ?? msg.id,
    [msg.id, originId?.id, ...stanzaIds.map((candidate) => candidate.id)],
  );
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
  extractExtensionAnnotations(msg, base);
  extractMarkup(msg, base);
  extractReplyAndThread(msg, base);
  stripReplyFallback(msg, base);

  if (ext(msg).sticker) {
    base.isSticker = true;
  }
}

interface ExtensionEnvelopePayload {
  annotations?: ExtensionAnnotationPayload | ExtensionAnnotationPayload[];
  enrichments?: ExtensionEnrichmentPayload | ExtensionEnrichmentPayload[];
}

interface ExtensionAnnotationPayload {
  extension?: string;
  id?: string;
  card?: ExtensionCardPayload;
}

interface ExtensionCardPayload {
  title?: string;
  summary?: string;
  image?: string;
  fields?: ExtensionFieldPayload | ExtensionFieldPayload[];
  actions?: ExtensionActionPayload | ExtensionActionPayload[];
}

interface ExtensionFieldPayload {
  name?: string;
  value?: string;
}

interface ExtensionActionPayload {
  route?: string;
  label?: string;
}

interface ExtensionEnrichmentPayload {
  id?: string;
  plugin?: string;
  capability?: string;
  payloadNamespace?: string;
  surface?: string;
  payloadSurface?: string;
  uiSurface?: string;
  payload?: ExtensionFallbackPayload;
  launches?: ExtensionLaunchDescriptorPayload | ExtensionLaunchDescriptorPayload[];
  source?: ExtensionSourcePayload;
}

interface ExtensionFallbackPayload {
  surface?: string;
  views?: ExtensionViewPayload | ExtensionViewPayload[];
  elements?: ExtensionRawElementPayload[];
}

interface ExtensionViewPayload {
  id?: string;
  title?: string;
  textBlocks?: ExtensionTextBlockPayload | ExtensionTextBlockPayload[];
}

interface ExtensionTextBlockPayload {
  text?: string;
  style?: string;
}

interface ExtensionLaunchDescriptorPayload {
  id?: string;
  plugin?: string;
  action?: string;
  commandNode?: string;
  token?: string;
  launchToken?: string;
  label?: string;
  context?: ExtensionLaunchContextPayload;
  expiresAt?: string;
  payload?: ExtensionFallbackPayload;
}

interface ExtensionSourcePayload {
  stanzaId?: string;
  by?: string;
  bodyStart?: string | number;
  bodyEnd?: string | number;
}

interface ExtensionLaunchContextPayload {
  waddleId?: string;
  room?: string;
  roomJid?: string;
  stanzaId?: string;
  sourceStanzaId?: string;
}

interface ExtensionRawElementPayload {
  name?: string;
  attributes?: Record<string, unknown>;
  children?: Array<ExtensionRawElementPayload | string>;
}

const ALLOWED_EXTENSION_SURFACES = new Set<ExtensionSurfaceKind>([
  "message-card",
  "board",
  "game",
  "chat-bot",
  "dynamic-canvas",
  "utility-panel",
]);

const FRAMEWORK_NAMESPACE = "urn:waddle:extension:1";

function parseExtensionAnnotation(payload: ExtensionAnnotationPayload): ExtensionAnnotation | null {
  if (!payload.extension || !payload.id || !payload.card?.title) return null;
  const fields = Object.fromEntries(
    asArray(payload.card.fields)
      .filter((field): field is Required<ExtensionFieldPayload> => !!field.name && typeof field.value === "string")
      .map((field) => [field.name, field.value]),
  );
  const surfaceKind = ALLOWED_EXTENSION_SURFACES.has(fields.surface as ExtensionSurfaceKind)
    ? fields.surface as ExtensionSurfaceKind
    : "message-card";
  return {
    extensionId: payload.extension,
    annotationId: payload.id,
    surfaceKind,
    title: payload.card.title,
    ...(payload.card.summary ? { summary: payload.card.summary } : {}),
    ...(payload.card.image ? { imageUrl: payload.card.image } : {}),
    fields,
    actions: asArray(payload.card.actions)
      .filter((action): action is Required<ExtensionActionPayload> => !!action.route && !!action.label)
      .map((action) => ({ route: action.route, label: action.label })),
  };
}

function extensionAnnotationsFromStanza(stanza: Record<string, unknown>): ExtensionAnnotation[] {
  const envelope = stanza.waddleExtensions as ExtensionEnvelopePayload | undefined;
  return [
    ...asArray(envelope?.annotations).flatMap((payload) => parseExtensionAnnotation(payload) ?? []),
    ...asArray(envelope?.enrichments).flatMap((payload) => parseExtensionEnrichment(payload) ?? []),
  ];
}

function parseExtensionEnrichment(payload: ExtensionEnrichmentPayload): ExtensionAnnotation | null {
  if (!payload.id || !payload.plugin) return null;
  const plugin = payload.plugin;
  const payloads = parsePayloadElements(payload.payload, payload.payloadNamespace);
  const source = parseExtensionSource(payload.source);
  const surfaceKind = inferSurfaceKind(payload, payloads);
  if (!surfaceKind) return null;
  const launches = asArray(payload.launches).flatMap((launch) =>
    parseExtensionLaunch(launch, plugin, source) ?? [],
  );
  const actions = asArray(payload.launches)
    .flatMap((launch) => {
      const parsed = launches.find((candidate) => candidate.id === launch.id);
      if (!parsed) return [];
      return [{ route: parsed.id, label: parsed.label, launch: parsed }];
    });
  return {
    extensionId: payload.plugin,
    annotationId: payload.id,
    surfaceKind,
    title: extensionTitle(payload, payloads),
    ...(payload.payloadNamespace ? { summary: payload.payloadNamespace } : {}),
    ...(source ? { source } : {}),
    ...(payload.payloadNamespace ? { payloadNamespace: payload.payloadNamespace } : {}),
    ...(payloads.length > 0 ? { payloads } : {}),
    fields: {
      ...(payload.capability ? { capability: payload.capability } : {}),
      ...(payload.payloadNamespace ? { payloadNamespace: payload.payloadNamespace } : {}),
      ...(payload.surface ? { surface: payload.surface } : {}),
      ...(payload.payloadSurface ? { payloadSurface: payload.payloadSurface } : {}),
      ...(payload.uiSurface ? { uiSurface: payload.uiSurface } : {}),
    },
    actions,
  };
}

function inferSurfaceKind(
  payload: ExtensionEnrichmentPayload,
  payloads: ExtensionPayloadElement[],
): ExtensionSurfaceKind | null {
  const explicitSurface = parseExtensionSurfaceKind(payload.surface)
    ?? parseExtensionSurfaceKind(payload.uiSurface)
    ?? parseExtensionSurfaceKind(payload.payloadSurface)
    ?? parseExtensionSurfaceKind(payload.payload?.surface)
    ?? payloads
      .map((element) => parseExtensionSurfaceKind(
        element.attributes.surface
          ?? element.attributes["ui-surface"]
          ?? element.attributes["payload-surface"],
      ))
      .find((surface): surface is ExtensionSurfaceKind => !!surface);
  if (explicitSurface) return explicitSurface;
  if (hasPayloadItems(payload.payload?.views)) return "message-card";
  if (payloads.length > 0) return "message-card";
  return null;
}

function parseExtensionSurfaceKind(value: unknown): ExtensionSurfaceKind | null {
  if (typeof value !== "string") return null;
  const surface = value.trim();
  if (ALLOWED_EXTENSION_SURFACES.has(surface as ExtensionSurfaceKind)) return surface as ExtensionSurfaceKind;
  if (surface === "message-enrichment" || surface === "pubsub-item") {
    return "message-card";
  }
  return null;
}

function hasPayloadItems(value: unknown[] | unknown | undefined): boolean {
  return Array.isArray(value) ? value.length > 0 : value !== undefined && value !== null;
}

function extensionTitle(payload: ExtensionEnrichmentPayload, payloads: ExtensionPayloadElement[]): string {
  const view = asArray(payload.payload?.views).find((candidate) => candidate?.title || hasPayloadItems(candidate?.textBlocks));
  if (view?.title) return view.title;
  const textBlock = view ? asArray(view.textBlocks).find((candidate) => typeof candidate?.text === "string" && candidate.text.trim().length > 0) : undefined;
  if (textBlock?.text) return textBlock.text;
  const payloadTitle = payloads
    .map((element) => readablePayloadTitle(element))
    .find((value) => value.length > 0);
  if (payloadTitle) return payloadTitle;
  return humanizeExtensionName(payload.plugin ?? "Extension");
}

function parseExtensionLaunch(
  payload: ExtensionLaunchDescriptorPayload,
  fallbackPluginId: string,
  source?: ExtensionEnvelopeSource,
): ExtensionLaunchDescriptor | null {
  if (!payload.id || !payload.label || !payload.commandNode) return null;
  const pluginId = payload.plugin ?? fallbackPluginId;
  const actionId = payload.action ?? payload.id;
  const context = payload.context ?? {};
  return {
    id: payload.id,
    pluginId,
    actionId,
    commandNode: payload.commandNode,
    ...(payload.launchToken ?? payload.token ? { launchToken: payload.launchToken ?? payload.token } : {}),
    label: payload.label,
    context: {
      ...(context.waddleId ? { waddleId: context.waddleId } : {}),
      ...(context.room ?? context.roomJid ? { roomJid: context.room ?? context.roomJid } : {}),
      ...(context.stanzaId ?? context.sourceStanzaId ? { stanzaId: context.stanzaId ?? context.sourceStanzaId } : {}),
    },
    ...(payload.expiresAt ? { expiresAt: payload.expiresAt } : {}),
    ...(source ? { source } : {}),
    payloads: parsePayloadElements(payload.payload),
  };
}

function parseExtensionSource(payload: ExtensionSourcePayload | undefined): ExtensionEnvelopeSource | undefined {
  if (!payload?.stanzaId) return undefined;
  const bodyStart = parseReferenceOffset(payload.bodyStart);
  const bodyEnd = parseReferenceOffset(payload.bodyEnd);
  return {
    stanzaId: payload.stanzaId,
    ...(payload.by ? { by: payload.by } : {}),
    ...(bodyStart !== undefined ? { bodyStart } : {}),
    ...(bodyEnd !== undefined ? { bodyEnd } : {}),
  };
}

function parsePayloadElements(
  payload: ExtensionFallbackPayload | undefined,
  fallbackNamespace?: string,
): ExtensionPayloadElement[] {
  return asArray(payload?.elements)
    .flatMap((element) => normalizePayloadElement(element, fallbackNamespace) ?? [])
    .filter((element) => element.namespace !== FRAMEWORK_NAMESPACE);
}

function normalizePayloadElement(
  payload: ExtensionRawElementPayload,
  fallbackNamespace = "",
): ExtensionPayloadElement | null {
  if (!payload?.name) return null;
  const rawAttributes = payload.attributes ?? {};
  const attributes = Object.fromEntries(
    Object.entries(rawAttributes)
      .filter(([, value]) => typeof value === "string" || typeof value === "number" || typeof value === "boolean")
      .map(([key, value]) => [key, String(value)]),
  );
  const namespace = attributes.xmlns ?? fallbackNamespace;
  const text = (payload.children ?? [])
    .filter((child): child is string => typeof child === "string")
    .join("")
    .trim();
  const children = (payload.children ?? [])
    .flatMap((child) => typeof child === "string" ? [] : normalizePayloadElement(child, namespace) ?? []);
  return {
    namespace,
    name: payload.name,
    attributes,
    ...(text ? { text } : {}),
    children,
  };
}

function readablePayloadTitle(element: ExtensionPayloadElement): string {
  for (const key of ["title", "label", "name", "question", "prompt"]) {
    const value = element.attributes[key]?.trim();
    if (value) return value;
  }
  for (const child of element.children) {
    const text = child.text?.trim();
    if (text) return text;
  }
  return humanizeExtensionName(element.name);
}

function humanizeExtensionName(value: string): string {
  return value
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[-_:#]+/g, " ")
    .trim()
    .replace(/\s+/g, " ")
    .replace(/\b\w/g, (char) => char.toUpperCase());
}

function hasNonBodyMessagePayload(msg: ReceivedMessage): boolean {
  const stanza = ext(msg);
  const fileSharing = stanza.fileSharing;
  return (Array.isArray(fileSharing) ? fileSharing.length > 0 : !!fileSharing)
    || !!stanza.sticker
    || extensionAnnotationsFromStanza(stanza).length > 0;
}

function extractExtensionAnnotations(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const annotations = extensionAnnotationsFromStanza(ext(msg));
  if (annotations.length > 0) base.extensionAnnotations = annotations;
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
    if (!base.threadId && base.id) base.threadId = base.id;
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
  const fallbacks = asArray(ext(msg).fallbacks as FallbackPayload | FallbackPayload[] | undefined);
  if (!fallbacks.length || !base.body) return;
  const range = fallbacks.find((f) => f.for === "urn:xmpp:reply:0")?.body;
  if (!range) return;
  const rawStart = range.start ?? 0;
  const rawEnd = range.end ?? rawStart;
  if (!Number.isFinite(rawStart) || !Number.isFinite(rawEnd) || rawStart < 0 || rawEnd < 0) {
    return;
  }
  const bodyLength = codePointLength(base.body);
  const start = Math.max(0, Math.min(rawStart, bodyLength));
  const end = Math.max(start, Math.min(rawEnd, bodyLength));
  if (end <= start) return;
  const startIndex = codePointToCodeUnitIndex(base.body, start);
  const endIndex = codePointToCodeUnitIndex(base.body, end);
  const prefixText = base.body.slice(0, startIndex);
  const strippedText = base.body.slice(startIndex, endIndex);
  const markupRangeStart = codePointLength(prefixText);
  const markupRangeEnd = markupRangeStart + codePointLength(strippedText);
  base.body = base.body.slice(0, startIndex) + base.body.slice(endIndex);
  if (base.markup?.length) {
    const rebasedMarkup = stripMarkupRange(base.markup, markupRangeStart, markupRangeEnd);
    if (rebasedMarkup.length > 0) {
      base.markup = rebasedMarkup;
    } else {
      delete base.markup;
    }
  }
  if (!base.references?.length) return;
  const rebasedReferences = stripReferenceRange(base.references, markupRangeStart, markupRangeEnd);
  if (rebasedReferences.length > 0) {
    base.references = rebasedReferences;
  } else {
    delete base.references;
  }
}

function rebaseOffsetAfterRemoval(offset: number, start: number, end: number): number {
  if (offset <= start) return offset;
  if (offset >= end) return offset - (end - start);
  return start;
}

function stripReferenceRange(references: readonly MessageReference[], start: number, end: number): MessageReference[] {
  return references.flatMap((reference) => {
    if (typeof reference.begin !== "number" || typeof reference.end !== "number") return [reference];
    const begin = rebaseOffsetAfterRemoval(reference.begin, start, end);
    const rebasedEnd = rebaseOffsetAfterRemoval(reference.end, start, end);
    return rebasedEnd > begin ? [{ ...reference, begin, end: rebasedEnd }] : [];
  });
}

function parseReferenceOffset(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value !== "string" || !value) return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : undefined;
}

function extractReferences(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const refs = ext(msg).references as Array<{ type?: string; uri?: string; begin?: string | number; end?: string | number; anchor?: string }> | undefined;
  if (!refs?.length) return;

  const mentionUris = refs
    .filter((r) => r.type === "mention" && r.uri)
    .map((r) => (r.uri as string).replace(/^xmpp:/, ""));
  if (mentionUris.length > 0) {
    base.mentions = mentionUris;
  }

  const references = refs.flatMap((r) => {
    if (!r.type || !r.uri) return [];
    const begin = parseReferenceOffset(r.begin);
    const end = parseReferenceOffset(r.end);
    return [{
      type: r.type,
      uri: r.uri,
      ...(begin !== undefined ? { begin } : {}),
      ...(end !== undefined ? { end } : {}),
      ...(r.anchor ? { anchor: r.anchor } : {}),
    }];
  });
  if (references.length > 0) base.references = references;
}

function extractExplicitMentions(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const mentions = ext(msg).explicitMentions as
    | Array<{ mentions?: string; active?: boolean }>
    | { mentions?: string; active?: boolean }
    | undefined;
  if (!mentions) return;

  for (const m of Array.isArray(mentions) ? mentions : [mentions]) {
    if (m.mentions === "urn:xmpp:mentions:0#channel") {
      base.broadcastMention = m.active ? "here" : "everyone";
      return;
    }
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

  const messageIds = resolveMessageIds(msg, roomJid);
  const applyTo = ext(msg).applyTo as { id?: string; moderated?: { retract?: boolean } } | undefined;
  if (applyTo?.id && applyTo.moderated) {
    const moderationMessage: LiveRoomMessage = {
      id: messageIds.id,
      roomJid,
      nick,
      body: "",
      createdAt: new Date().toISOString(),
      type: "message",
      retractsId: applyTo.id,
    };
    if (messageIds.wireIds?.length) moderationMessage.wireIds = messageIds.wireIds;
    h.onMessage?.(moderationMessage);
    return;
  }

  const retract = ext(msg).retract as { id?: string } | undefined;
  if (retract?.id) {
    const retractionMessage: LiveRoomMessage = {
      id: messageIds.id,
      roomJid,
      nick,
      body: "",
      createdAt: new Date().toISOString(),
      type: "message",
      retractsId: retract.id,
    };
    if (messageIds.wireIds?.length) retractionMessage.wireIds = messageIds.wireIds;
    h.onMessage?.(retractionMessage);
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

  if (!hasRenderableMessagePayload(msg)) return;

  const liveMsg: LiveRoomMessage = {
    id: messageIds.id, roomJid, nick,
    body: msg.body ?? msg.subject ?? "",
    createdAt: new Date().toISOString(),
    type: msg.body || hasNonBodyMessagePayload(msg) ? "message" : "subject",
  };
  const authorRealJid = extractMucUserRealJid(msg);
  if (authorRealJid) liveMsg.authorRealJid = authorRealJid;
  if (messageIds.wireIds?.length) liveMsg.wireIds = messageIds.wireIds;
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
  const markupData = ext(msg).markup as { spans?: import("@/lib/chat-ui").MarkupSpan[] } | undefined;
  if (!markupData?.spans || markupData.spans.length === 0) return;

  base.markup = markupData.spans;
}

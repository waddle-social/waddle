import { withSpan } from "@/lib/telemetry";
import { inferredFileDisposition } from "@/lib/chat-ui";
import type { MemberSummary, UserSearchResult } from "../chat-types";
import type { WaddleSession } from "../server-auth";
import {
  countQueuedMessages,
  enqueueQueuedMessage,
  listQueuedMessages,
  listQueuedRoomMessages,
  removeQueuedMessage,
  type PersistedQueuedDmMessage,
} from "../outbound-queue-store";
import { barePeerJid, jidDomain, roomBareJidFor } from "./jid";
import type {
  ChatStateEvent,
  ChatStateType,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  DiscoveredTopology,
  ListRoomMembersOptions,
  LiveDmMessage,
  LiveRoomMessage,
  MamHistoryPage,
  MamPageParam,
  MessageSearchResult,
  OccupantPresence,
  PresenceUpdateEvent,
  ReactionEvent,
  RoomActivityEvent,
  RoomHats,
  RoomPresence,
  RosterContact,
  SessionLifecycleEvent,
  SharedFileInfo,
  XmppErrorEvent,
  XmppStatusSnapshot,
} from "./types";
import { mergeOccupantHats, roleHatsForOccupant } from "./occupant-badges";
import { prepareEncryptedAttachmentUpload } from "./encrypted-attachments";
import { discoverChannels, discoverTopology } from "./discovery";
import { discoverUploadService, uploadFile, type UploadProgress } from "./file-upload";
import {
  discoverExtensionCommands,
  discoverExtensionRoutes,
  fetchExtensionRouteItems,
  invokeExtensionCommand,
  invokeExtensionLaunch,
  submitExtensionCommandForm,
  type DiscoveredExtensionCommand,
  type DiscoveredExtensionRoute,
  type ExtensionCommandAction,
  type ExtensionCommandFormField,
  type ExtensionCommandResult,
  type ExtensionRouteItem,
} from "./extension-commands";
import type { FetchInboxOptions, InboxEntry, InboxResult } from "./inbox-types";
import type { ActivityPublication, MoodPublication, TunePublication, UserPepProfile } from "./pep-types";
import {
  buildReplyFallbackPrefix,
  shiftMarkupSpans,
  type OutboundFileAttachment,
  type ReplyTarget,
  type SendDirectMessageOptions,
  type SendGroupMessageOptions,
} from "./send-types";
import type {
  WasmArchivedMessage,
  WasmAvatar,
  WasmInboxConversation,
  WasmMamPage,
  WasmMessage,
  WasmPepProfile,
  WasmPresence,
  WasmRoomMember,
  WasmRosterContact,
  WasmSendOptions,
  WasmServerVersion,
  WasmSharedFile,
  WasmUserSearchResult,
} from "./wasm-types";

type WasmModule = typeof import("@waddle/xmpp-client-wasm");
type WasmClient = import("@waddle/xmpp-client-wasm").WaddleClient;

type CompatEmitter = {
  on?: (event: string, handler: (...args: any[]) => void) => void;
  off?: (event: string, handler: (...args: any[]) => void) => void;
};

type CompatXmpp = Partial<WasmClient> & CompatEmitter & {
  sendMessage?: (message: Record<string, unknown>) => void;
  sendIQ?: (iq: Record<string, unknown>) => Promise<unknown>;
  joinRoom?: (roomJid: string, nick: string) => Promise<void>;
  leaveRoom?: (roomJid: string, nick: string) => Promise<void>;
  set_on_connected?: (cb: () => void) => void;
  set_on_disconnected?: (cb: () => void) => void;
  set_on_error?: (cb: (error: string) => void) => void;
  set_on_message?: (cb: (message: WasmMessage) => void) => void;
  set_on_presence?: (cb: (presence: WasmPresence) => void) => void;
  set_on_message_delivery_acked?: (cb: (id: string) => void) => void;
  set_on_message_delivery_failed?: (cb: (id: string) => void) => void;
  set_on_session_lifecycle?: (cb: (event: string) => void) => void;
};

interface OutboundSendResult {
  id: string | null;
  state: "queued" | "sending";
}

let wasmModulePromise: Promise<WasmModule> | null = null;

function createXmppResource() {
  const randomId = globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
  return `web-${randomId}`;
}

function parsePresenceShow(show: string | undefined): OccupantPresence {
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

function mapPresenceShow(presence: WasmPresence): PresenceUpdateEvent["show"] {
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

function avatarDataUrl(data: Uint8Array, mediaType?: string | null): string | null {
  if (!data?.length) return null;
  return `data:${mediaType || "image/png"};base64,${bufferLikeToBase64(data)}`;
}


function wasmSpanToMarkupSpan(span: WasmMessage["markup_spans"][number]): import("@/lib/rich-message").MarkupSpan | null {
  type MarkupSpan = import("@/lib/rich-message").MarkupSpan;
  type RichInlineStyle = import("@/lib/rich-message").RichInlineStyle;
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

function stripReplyFallback<T extends { body: string; markup?: Array<{ start: number; end: number }> }>(
  message: T,
  start?: number,
  end?: number,
): T {
  if (typeof start !== "number" || typeof end !== "number" || end <= start || !message.body) return message;
  const points = Array.from(message.body);
  const strippedBody = `${points.slice(0, start).join("")}${points.slice(end).join("")}`;
  const markup = message.markup?.length ? stripMarkupRange(message.markup, start, end) : undefined;
  return { ...message, body: strippedBody, ...(markup?.length ? { markup } : {}) };
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
  };
}

function roomMessageFromArchived(message: WasmArchivedMessage): LiveRoomMessage | null {
  const roomJid = barePeerJid(message.from ?? message.to ?? "");
  const nick = (message.from ?? "").split("/")[1] ?? "unknown";
  const createdAt = message.timestamp ?? new Date().toISOString();
  if (message.retracts_id) {
    return {
      id: message.id ?? message.stanza_id ?? message.mam_id,
      roomJid,
      nick,
      body: "",
      createdAt,
      type: "message",
      retractsId: message.retracts_id,
    };
  }
  if (message.reaction_target_id) {
    return {
      id: message.id ?? message.stanza_id ?? message.mam_id,
      roomJid,
      nick,
      body: "",
      createdAt,
      type: "subject",
      _reactionTarget: message.reaction_target_id,
      _reactionEmojis: message.reaction_emojis,
      ...(message.author_real_jid ? { _reactionSenderId: message.author_real_jid } : {}),
    };
  }
  const sharedFiles = message.shared_files ?? [];
  const mentionUris = message.mention_uris ?? [];
  const markupSpans = message.markup_spans ?? [];
  if (!message.body && !message.subject && !sharedFiles.length && !message.thread && !message.forum_post_kind) {
    return null;
  }
  const base: LiveRoomMessage = {
    id: message.id ?? message.stanza_id ?? message.mam_id,
    roomJid,
    nick,
    body: message.body ?? message.subject ?? "",
    createdAt,
    type: message.subject && !message.body ? "subject" : "message",
    ...(message.moderation_target_id ? { retractsId: message.moderation_target_id } : {}),
    ...(message.replaces_id ? { replacesId: message.replaces_id } : {}),
    ...(message.stanza_id || message.origin_id ? { wireIds: [message.id, message.origin_id, message.stanza_id].filter((value): value is string => !!value) } : {}),
    ...(message.reply_to_id ? { replyTo: { id: message.reply_to_id, ...(message.reply_to_sender ? { author: message.reply_to_sender } : {}) } } : {}),
    ...(message.thread ? { threadId: message.thread } : {}),
    ...(message.parent_thread_id ? { parentThreadId: message.parent_thread_id } : {}),
    ...(message.forum_post_kind === "topic" || message.forum_post_kind === "reply" ? { forumPostKind: message.forum_post_kind } : {}),
    ...(message.forum_title ? { forumTitle: message.forum_title } : {}),
    ...(message.forum_thread_title ? { forumThreadTitle: message.forum_thread_title } : {}),
    ...(message.author_real_jid ? { authorRealJid: message.author_real_jid } : {}),
    ...(message.reaction_target_id ? { reactionTargetId: message.reaction_target_id } : {}),
    ...(mentionUris.length ? { mentions: mentionUris.map((uri) => uri.replace(/^xmpp:/, "")) } : {}),
    ...(message.broadcast_mention === "here" || message.broadcast_mention === "everyone" ? { broadcastMention: message.broadcast_mention } : {}),
    ...(markupSpans.length ? { markup: markupSpans.flatMap((s) => { const m = wasmSpanToMarkupSpan(s); return m ? [m] : []; }) } : {}),
    ...(sharedFiles.length ? { sharedFiles: sharedFiles.map(sharedFileFromWasm) } : {}),
    ...(message.is_sticker ? { isSticker: true } : {}),
    ...(message.stanza_id ?? message.origin_id ? { correctionTargetId: message.origin_id ?? message.id ?? "" } : {}),
    ...(message.stanza_id ? { replyableId: message.stanza_id } : {}),
  };
  return stripReplyFallback(base, message.reply_fallback_start, message.reply_fallback_end);
}

function dmMessageFromArchived(message: WasmArchivedMessage, selfBareJid: string): LiveDmMessage | null {
  const fromBare = barePeerJid(message.from ?? "");
  const toBare = barePeerJid(message.to ?? "");
  const isSelf = fromBare === selfBareJid;
  const peerJid = isSelf ? toBare : fromBare;
  if (!peerJid) return null;
  const fromJid = message.from ?? fromBare;
  const nick = barePeerJid(fromJid).split("@")[0] ?? "unknown";
  const createdAt = message.timestamp ?? new Date().toISOString();
  if (message.retracts_id) {
    return {
      id: message.id ?? message.stanza_id ?? message.mam_id,
      peerJid,
      fromJid,
      nick,
      body: "",
      createdAt,
      type: "message",
      retractsId: message.retracts_id,
    };
  }
  if (message.reaction_target_id) {
    return {
      id: message.id ?? message.stanza_id ?? message.mam_id,
      peerJid,
      fromJid,
      nick,
      body: "",
      createdAt,
      type: "message",
      _reactionTarget: message.reaction_target_id,
      _reactionEmojis: message.reaction_emojis,
    };
  }
  const sharedFiles = message.shared_files ?? [];
  const mentionUris = message.mention_uris ?? [];
  const markupSpans = message.markup_spans ?? [];
  if (!message.body && !message.subject && !sharedFiles.length && !message.thread) return null;
  const base: LiveDmMessage = {
    id: message.id ?? message.stanza_id ?? message.mam_id,
    peerJid,
    fromJid,
    nick,
    body: message.body ?? message.subject ?? "",
    createdAt,
    type: "message",
    ...(message.replaces_id ? { replacesId: message.replaces_id } : {}),
    ...(message.reply_to_id ? { replyTo: { id: message.reply_to_id, ...(message.reply_to_sender ? { author: message.reply_to_sender } : {}) } } : {}),
    ...(message.thread ? { threadId: message.thread } : {}),
    ...(message.parent_thread_id ? { parentThreadId: message.parent_thread_id } : {}),
    ...(message.forum_post_kind === "topic" || message.forum_post_kind === "reply" ? { forumPostKind: message.forum_post_kind } : {}),
    ...(message.forum_title ? { forumTitle: message.forum_title } : {}),
    ...(message.forum_thread_title ? { forumThreadTitle: message.forum_thread_title } : {}),
    ...(mentionUris.length ? { mentions: mentionUris.map((uri) => uri.replace(/^xmpp:/, "")) } : {}),
    ...(markupSpans.length ? { markup: markupSpans.flatMap((s) => { const m = wasmSpanToMarkupSpan(s); return m ? [m] : []; }) } : {}),
    ...(sharedFiles.length ? { sharedFiles: sharedFiles.map(sharedFileFromWasm) } : {}),
    ...(message.is_sticker ? { isSticker: true } : {}),
    ...(message.origin_id || message.id ? { correctionTargetId: message.origin_id ?? message.id ?? "" } : {}),
    ...(message.origin_id || message.id ? { replyableId: message.origin_id ?? message.id ?? undefined } : {}),
    ...(message.stanza_id || message.origin_id ? { wireIds: [message.id, message.origin_id, message.stanza_id].filter((value): value is string => !!value) } : {}),
  };
  return stripReplyFallback(base, message.reply_fallback_start, message.reply_fallback_end);
}

function inboxEntryFromWasm(conversation: WasmInboxConversation): InboxEntry {
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

function buildWasmSendOptions(opts: SendGroupMessageOptions | SendDirectMessageOptions, replyFallbackLength: number): WasmSendOptions {
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
    }));
  }
  if (opts.markup?.length) {
    type RichInlineStyle = import("@/lib/rich-message").RichInlineStyle;
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
    }));
  }
  return wasmOpts;
}

function encodeBodyForSend(body: string, replyTo?: ReplyTarget, markup?: SendGroupMessageOptions["markup"]) {
  const { prefix, length } = replyTo ? buildReplyFallbackPrefix(replyTo.body) : { prefix: "", length: 0 };
  return {
    effectiveBody: `${prefix}${body}`,
    replyFallbackLength: length,
    rebasedMarkup: markup?.length ? shiftMarkupSpans(markup, length) : undefined,
  };
}

async function loadWasmModule(): Promise<WasmModule> {
  if (!wasmModulePromise) {
    wasmModulePromise = import("@waddle/xmpp-client-wasm").then(async (mod) => {
      if (typeof mod.default === "function") {
        await mod.default();
      }
      return mod;
    });
  }
  return wasmModulePromise;
}

export class BrowserXmppClient {
  private session: WaddleSession;
  private get queueScope() { return barePeerJid(this.session.jid); }
  private readonly resource = createXmppResource();
  private messageHandler: ((message: LiveRoomMessage) => void) | null = null;
  private directMessageHandler: ((message: LiveDmMessage) => void) | null = null;
  private statusHandler: ((status: XmppStatusSnapshot) => void) | null = null;
  private reactionHandler: ((event: ReactionEvent) => void) | null = null;
  private displayedHandler: ((event: { roomJid: string; nick: string; messageId: string }) => void) | null = null;
  private chatStateHandler: ((event: ChatStateEvent) => void) | null = null;
  private dmChatStateHandler: ((event: DmChatStateEvent) => void) | null = null;
  private dmReactionHandler: ((event: DmReactionEvent) => void) | null = null;
  private dmDisplayedHandler: ((event: DmDisplayedEvent) => void) | null = null;
  private presenceUpdateHandler: ((event: PresenceUpdateEvent) => void) | null = null;
  private roomMemberJids: Record<string, string> = {};
  private memberJidHandler: ((nick: string, bareJid: string) => void) | null = null;
  private hatsHandler: ((hats: RoomHats) => void) | null = null;
  private slowModeHandler: ((seconds: number) => void) | null = null;
  private activityHandler: ((event: RoomActivityEvent) => void) | null = null;
  private inboxPushHandler: ((entry: InboxEntry) => void) | null = null;
  private roomAvatarHandler: ((roomJid: string, hash: string) => void) | null = null;
  private roomDisconnectHandler: (() => void) | null = null;
  private presenceHandler: ((presence: RoomPresence) => void) | null = null;
  private lastSeenHandler: ((nick: string, timestamp: number) => void) | null = null;
  private messageAckHandler: ((messageId: string) => void) | null = null;
  private messageDeliveryFailureHandler: ((messageId: string) => void) | null = null;
  private queuedMessageStatusHandler: ((messageId: string, status: "queued" | "sending") => void) | null = null;
  private sessionLifecycleHandler: ((event: SessionLifecycleEvent) => void) | null = null;
  private xmpp: CompatXmpp | null = null;
  private connectPromise: Promise<void> | null = null;
  private connected = false;
  private destroying = false;
  private refreshSession: (() => Promise<WaddleSession | null>) | null = null;
  private currentRoom: string | null = null;
  private roomSwitchPromise: Promise<void> | null = null;
  private roomSwitchTarget: string | null = null;
  private selfPingTimer: ReturnType<typeof setInterval> | null = null;
  private roomHats: RoomHats = {};
  private roomPresence: RoomPresence = {};
  private uploadServiceJid: string | null = null;
  private discoveredRoomJids = new Map<string, string>();
  private reconnectStartedAt: number | null = null;
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private directQueueFlushPromise: Promise<void> | null = null;
  private readonly roomQueueFlushes = new Map<string, Promise<void>>();
  private readonly inflightQueuedIds = new Set<string>();
  private readonly pendingSendAt = new Map<string, { at: number; kind: "room" | "dm" }>();
  private readonly messageAckHooks: Array<(id: string, meta: { kind: "room" | "dm"; latencyMs: number }) => void> = [];
  private readonly messageFailHooks: Array<(id: string, meta: { kind: "room" | "dm" }) => void> = [];
  private readonly sessionLifecycleHooks: Array<(event: SessionLifecycleEvent) => void> = [];
  private readonly statusHooks: Array<(status: XmppStatusSnapshot, meta: { reconnectDurationMs?: number }) => void> = [];
  private readonly sendEnqueuedHooks: Array<(info: { kind: "room" | "dm"; reason: string }) => void> = [];
  private readonly queueDepthHooks: Array<(depth: { persisted: number; inflight: number }) => void> = [];
  private readonly errorHooks: Array<(event: XmppErrorEvent) => void> = [];
  private readonly roomJoinWaiters = new Map<string, { resolve: () => void; reject: (error: Error) => void }>();
  private readonly carbonDedupIds = new Set<string>();
  readonly catchup = {
    latestDmSeenAt: new Map<string, string>(),
    primed: false,
    recordDmSeen: (peer: string, timestamp: string) => {
      this.catchup.latestDmSeenAt.set(barePeerJid(peer), timestamp);
    },
    onSessionStarted: () => {
      const shouldRun = this.catchup.primed;
      this.catchup.primed = true;
      return shouldRun ? Array.from(this.catchup.latestDmSeenAt.entries()) : [];
    },
  };

  constructor(session: WaddleSession) { this.session = session; }

  setMessageHandler(h: (message: LiveRoomMessage) => void) { this.messageHandler = h; }
  setDirectMessageHandler(h: (message: LiveDmMessage) => void) { this.directMessageHandler = h; }
  setStatusHandler(h: (status: XmppStatusSnapshot) => void) { this.statusHandler = h; }
  setChatStateHandler(h: (event: ChatStateEvent) => void) { this.chatStateHandler = h; }
  setDmChatStateHandler(h: (event: DmChatStateEvent) => void) { this.dmChatStateHandler = h; }
  setReactionHandler(h: (event: ReactionEvent) => void) { this.reactionHandler = h; }
  setDmReactionHandler(h: (event: DmReactionEvent) => void) { this.dmReactionHandler = h; }
  setDisplayedHandler(h: (event: { roomJid: string; nick: string; messageId: string }) => void) { this.displayedHandler = h; }
  setDmDisplayedHandler(h: (event: DmDisplayedEvent) => void) { this.dmDisplayedHandler = h; }
  setPresenceUpdateHandler(h: (event: PresenceUpdateEvent) => void) { this.presenceUpdateHandler = h; }
  setMemberJidHandler(h: (nick: string, bareJid: string) => void) { this.memberJidHandler = h; }
  setHatsHandler(h: (hats: RoomHats) => void) { this.hatsHandler = h; }
  setSlowModeHandler(h: (seconds: number) => void) { this.slowModeHandler = h; }
  setActivityHandler(h: (event: RoomActivityEvent) => void) { this.activityHandler = h; }
  setInboxPushHandler(h: (entry: InboxEntry) => void) { this.inboxPushHandler = h; }
  setRoomAvatarHandler(h: (roomJid: string, hash: string) => void) { this.roomAvatarHandler = h; }
  setRoomDisconnectHandler(h: () => void) { this.roomDisconnectHandler = h; }
  setPresenceHandler(h: (presence: RoomPresence) => void) { this.presenceHandler = h; }
  setLastSeenHandler(h: (nick: string, timestamp: number) => void) { this.lastSeenHandler = h; }
  setMessageAckHandler(h: (messageId: string) => void) { this.messageAckHandler = h; }
  setMessageDeliveryFailureHandler(h: (messageId: string) => void) { this.messageDeliveryFailureHandler = h; }
  setQueuedMessageStatusHandler(h: (messageId: string, status: "queued" | "sending") => void) { this.queuedMessageStatusHandler = h; }
  setSessionLifecycleHandler(h: (event: SessionLifecycleEvent) => void) { this.sessionLifecycleHandler = h; }
  setRefreshSession(fn: () => Promise<WaddleSession | null>) { this.refreshSession = fn; }

  onMessageAcked(hook: (id: string, meta: { kind: "room" | "dm"; latencyMs: number }) => void) { this.messageAckHooks.push(hook); }
  onMessageDeliveryFailed(hook: (id: string, meta: { kind: "room" | "dm" }) => void) { this.messageFailHooks.push(hook); }
  onSessionLifecycle(hook: (event: SessionLifecycleEvent) => void) { this.sessionLifecycleHooks.push(hook); }
  onStatus(hook: (status: XmppStatusSnapshot, meta: { reconnectDurationMs?: number }) => void) { this.statusHooks.push(hook); }
  onSendEnqueued(hook: (info: { kind: "room" | "dm"; reason: string }) => void) { this.sendEnqueuedHooks.push(hook); }
  onQueueDepthChange(hook: (depth: { persisted: number; inflight: number }) => void) { this.queueDepthHooks.push(hook); }
  onError(hook: (event: XmppErrorEvent) => void) { this.errorHooks.push(hook); }

  private fireHook<Args extends unknown[]>(hooks: Array<(...args: Args) => void>, ...args: Args) {
    for (const hook of hooks) {
      try { hook(...args); } catch (error) { console.error("xmpp telemetry hook threw", error); }
    }
  }

  private emitError(event: XmppErrorEvent) { this.fireHook(this.errorHooks, event); }

  private updateReconnectTimer(snap: XmppStatusSnapshot): { reconnectDurationMs?: number } {
    if (snap.state === "reconnecting") {
      if (this.reconnectStartedAt === null) this.reconnectStartedAt = performance.now();
      return {};
    }
    if (this.reconnectStartedAt === null) return {};
    if (snap.state === "online") {
      const durationMs = performance.now() - this.reconnectStartedAt;
      this.reconnectStartedAt = null;
      return { reconnectDurationMs: durationMs };
    }
    this.reconnectStartedAt = null;
    return {};
  }

  private emitStatus(snapshot: XmppStatusSnapshot) {
    this.statusHandler?.(snapshot);
    const meta = this.updateReconnectTimer(snapshot);
    this.fireHook(this.statusHooks, snapshot, meta);
  }

  private emitSessionLifecycle(event: SessionLifecycleEvent) {
    this.sessionLifecycleHandler?.(event);
    this.fireHook(this.sessionLifecycleHooks, event);
  }

  private emitQueueDepth() {
    this.fireHook(this.queueDepthHooks, {
      persisted: countQueuedMessages(this.queueScope),
      inflight: this.inflightQueuedIds.size,
    });
  }

  private recordPendingSend(id: string | null, kind: "room" | "dm") {
    if (!id) return;
    this.pendingSendAt.set(id, { at: performance.now(), kind });
  }

  private clearReconnectTimer() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private scheduleReconnect() {
    if (this.destroying || this.reconnectTimer) return;
    const delay = Math.min(2000 * (2 ** this.reconnectAttempt), 60000);
    this.reconnectAttempt += 1;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      void this.connect().catch(() => undefined);
    }, delay);
  }

  private async enableCarbons(xmpp: CompatXmpp & { enableCarbons?: () => Promise<void> }) {
    if (xmpp.enableCarbons) {
      try { await xmpp.enableCarbons(); } catch {}
      return;
    }
    if (!xmpp.send_raw_iq) return;
    try { await xmpp.send_raw_iq(`<iq type="set" id="${crypto.randomUUID()}"><enable xmlns="urn:xmpp:carbons:2"/></iq>`); } catch {}
  }

  private async refreshRosterPresenceSubscriptions(xmpp: CompatXmpp & { getRoster?: () => Promise<{ items?: Array<{ jid: string }> }> }) {
    try {
      if (xmpp.list_roster_contacts && xmpp.subscribe_to_presence) {
        const contacts = await xmpp.list_roster_contacts() as WasmRosterContact[];
        await Promise.all(contacts.map((contact) => xmpp.subscribe_to_presence?.(contact.jid)));
        return;
      }
      if (xmpp.getRoster && xmpp.subscribe_to_presence) {
        const roster = await xmpp.getRoster();
        await Promise.all((roster.items ?? []).map((contact) => xmpp.subscribe_to_presence?.(contact.jid)));
      }
    } catch {}
  }

  private async doConnect(): Promise<void> {
    const mod = await loadWasmModule();
    const config = new mod.WaddleConfig(
      this.session.xmpp_websocket_url,
      this.session.jid,
      this.session.session_id,
      this.resource,
    );
    const xmpp = new mod.WaddleClient(config) as unknown as CompatXmpp;
    this.xmpp = xmpp;
    this.wireEvents(xmpp);
    await xmpp.connect?.();
  }

  private onceConnected: (() => void) | null = null;
  private onceConnectFailed: ((error: Error) => void) | null = null;

  async connect(): Promise<void> {
    if (this.xmpp && this.connected) return;
    if (this.connectPromise) return this.connectPromise;
    this.destroying = false;
    this.clearReconnectTimer();
    this.connectPromise = withSpan(
      "xmpp.connect",
      { "waddle.xmpp.jid": this.session.jid, "waddle.xmpp.transport": "websocket" },
      () => new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => {
          this.connectPromise = null;
          reject(new Error("Reconnection timed out"));
        }, 15000);
        const done = (fn: () => void) => {
          clearTimeout(timeout);
          this.connectPromise = null;
          fn();
        };
        this.onceConnected = () => done(resolve);
        this.onceConnectFailed = (error) => done(() => reject(error));
        void this.doConnect().catch((error) => {
          if (this.onceConnectFailed) {
            const fail = this.onceConnectFailed;
            this.onceConnectFailed = null;
            fail(error instanceof Error ? error : new Error(String(error)));
          }
        });
      }),
    );
    return this.connectPromise;
  }

  async disconnect() {
    this.destroying = true;
    this.clearReconnectTimer();
    const xmpp = this.xmpp;
    const roomBefore = this.currentRoom;
    this.stopSelfPing();
    this.connected = false;
    this.connectPromise = null;
    this.currentRoom = null;
    this.roomSwitchPromise = null;
    this.roomSwitchTarget = null;
    this.uploadServiceJid = null;
    this.roomHats = {};
    this.roomPresence = {};
    this.roomMemberJids = {};
    this.xmpp = null;
    try {
      if (xmpp && roomBefore && xmpp.leave_room) {
        await xmpp.leave_room(roomBefore, this.session.username);
      }
    } catch {}
    await xmpp?.disconnect?.();
    this.emitStatus({ state: "offline", detail: "Disconnected" });
  }

  private roomJidForChannel(channelId: string): string {
    return this.discoveredRoomJids.get(channelId) ?? roomBareJidFor(this.session, channelId);
  }

  private waitForRoomSelfPresence(roomJid: string, nick: string): Promise<void> {
    const fullJid = `${roomJid}/${nick}`;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.roomJoinWaiters.delete(fullJid);
        reject(new Error(`Timed out waiting for self-presence in ${roomJid}`));
      }, 4000);
      this.roomJoinWaiters.set(fullJid, {
        resolve: () => {
          clearTimeout(timeout);
          this.roomJoinWaiters.delete(fullJid);
          resolve();
        },
        reject: (error) => {
          clearTimeout(timeout);
          this.roomJoinWaiters.delete(fullJid);
          reject(error);
        },
      });
    });
  }

  async switchRoom(_spaceId: string, channelId: string) {
    await this.connect();
    const nextRoom = this.roomJidForChannel(channelId);
    if (this.roomSwitchPromise) {
      if (this.roomSwitchTarget === nextRoom) return this.roomSwitchPromise;
      await this.roomSwitchPromise.catch(() => undefined);
    }
    if (this.currentRoom === nextRoom) return;
    const promise = this.performRoomSwitch(nextRoom);
    this.roomSwitchPromise = promise;
    this.roomSwitchTarget = nextRoom;
    try {
      await promise;
    } finally {
      if (this.roomSwitchPromise === promise) {
        this.roomSwitchPromise = null;
        this.roomSwitchTarget = null;
      }
    }
  }

  private async performRoomSwitch(nextRoom: string) {
    const xmpp = this.xmpp;
    if (!xmpp) return;
    if (this.currentRoom && xmpp.leave_room) {
      try { await xmpp.leave_room(this.currentRoom, this.session.username); } catch {}
    }
    this.currentRoom = nextRoom;
    this.roomHats = {};
    this.roomPresence = {};
    this.roomMemberJids = {};
    this.hatsHandler?.({});
    this.presenceHandler?.({});
    if (xmpp.join_room) {
      const ready = this.waitForRoomSelfPresence(nextRoom, this.session.username);
      await Promise.allSettled([xmpp.join_room(nextRoom, this.session.username)]);
      await ready;
    } else if (xmpp.joinRoom) {
      await xmpp.joinRoom(nextRoom, this.session.username);
    }
    this.startSelfPing();
    await this.flushQueuedRoomMessages(nextRoom);
  }

  private browserOffline(): boolean {
    return typeof navigator !== "undefined" && navigator.onLine === false;
  }

  private canUseConnectedSession(): boolean {
    return !!this.xmpp && this.connected && !this.destroying && !this.browserOffline();
  }

  private roomIsReady(roomJid: string): boolean {
    return this.canUseConnectedSession() && this.currentRoom === roomJid;
  }

  private enqueueReason(): string {
    if (this.browserOffline()) return "offline";
    if (this.destroying) return "destroying";
    if (!this.xmpp) return "no-client";
    if (!this.connected) return "reconnecting";
    return "not-ready";
  }

  private noteQueuedMessage() {
    const queueCount = countQueuedMessages(this.queueScope);
    this.emitStatus({
      state: this.browserOffline() ? "offline" : "reconnecting",
      detail: queueCount === 1 ? "Message queued until the connection returns" : `${queueCount} messages queued until the connection returns`,
    });
  }

  private queueRoomMessage(roomJid: string, body: string, opts: SendGroupMessageOptions): OutboundSendResult {
    const queuedId = opts.id ?? crypto.randomUUID();
    enqueueQueuedMessage(this.queueScope, {
      kind: "room",
      id: queuedId,
      createdAt: new Date().toISOString(),
      roomJid,
      body,
      ...(opts.markup?.length ? { markup: opts.markup } : {}),
      ...(opts.references?.length ? { references: opts.references } : {}),
      ...(opts.mentionJidsByNick ? { mentionJidsByNick: opts.mentionJidsByNick } : {}),
      ...(opts.files?.length ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
      ...(opts.threadCreate ? { threadCreate: opts.threadCreate } : {}),
      ...(opts.threadReply ? { threadReply: opts.threadReply } : {}),
    });
    this.queuedMessageStatusHandler?.(queuedId, "queued");
    this.noteQueuedMessage();
    this.fireHook(this.sendEnqueuedHooks, { kind: "room", reason: this.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  private queueDirectMessage(peerJid: string, body: string, opts: SendDirectMessageOptions): OutboundSendResult {
    const queuedId = opts.id ?? crypto.randomUUID();
    enqueueQueuedMessage(this.queueScope, {
      kind: "dm",
      id: queuedId,
      createdAt: new Date().toISOString(),
      peerJid: barePeerJid(peerJid),
      body,
      ...(opts.markup?.length ? { markup: opts.markup } : {}),
      ...(opts.references?.length ? { references: opts.references } : {}),
      ...(opts.files?.length ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
    });
    this.queuedMessageStatusHandler?.(queuedId, "queued");
    this.noteQueuedMessage();
    this.fireHook(this.sendEnqueuedHooks, { kind: "dm", reason: this.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  private async compatSendGroupMessage(xmpp: CompatXmpp, roomJid: string, body: string, opts: SendGroupMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup } = encodeBodyForSend(body, opts.replyTo, opts.markup);
    const wasmOpts = buildWasmSendOptions({ ...opts, markup: rebasedMarkup }, replyFallbackLength);
    if (xmpp.send_groupchat_message) return await xmpp.send_groupchat_message(roomJid, effectiveBody, wasmOpts) as string;
    if (xmpp.sendMessage) {
      xmpp.sendMessage({ id: wasmOpts.stanza_id, to: roomJid, type: "groupchat", body: effectiveBody, ...(wasmOpts.subject ? { subject: wasmOpts.subject } : {}), ...(opts.threadId ? { thread: opts.threadId } : {}), ...(opts.threadCreate ? { threadCreate: opts.threadCreate } : {}), ...(opts.threadReply ? { threadReply: opts.threadReply } : {}) });
      return wasmOpts.stanza_id ?? null;
    }
    throw new Error("XMPP session is not ready");
  }

  private async compatSendDirectMessage(xmpp: CompatXmpp, peerJid: string, body: string, opts: SendDirectMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup } = encodeBodyForSend(body, opts.replyTo, opts.markup);
    const wasmOpts = buildWasmSendOptions({ ...opts, markup: rebasedMarkup }, replyFallbackLength);
    if (xmpp.send_chat_message) return await xmpp.send_chat_message(peerJid, effectiveBody, wasmOpts) as string;
    if (xmpp.sendMessage) {
      xmpp.sendMessage({ id: wasmOpts.stanza_id, to: peerJid, type: "chat", body: effectiveBody });
      return wasmOpts.stanza_id ?? null;
    }
    throw new Error("XMPP session is not ready");
  }

  private async compatSendChatState(xmpp: CompatXmpp, to: string, type: "chat" | "groupchat", state: ChatStateType) {
    if (xmpp.send_chat_state) return xmpp.send_chat_state(to, type, state);
    if (xmpp.sendMessage) { xmpp.sendMessage({ to, type, chatState: state }); return; }
    throw new Error("XMPP session is not ready");
  }

  private async compatSendDisplayed(xmpp: CompatXmpp, to: string, type: "chat" | "groupchat", id: string) {
    if (xmpp.send_displayed) return xmpp.send_displayed(to, type, id);
    if (xmpp.sendMessage) { xmpp.sendMessage({ to, type, marker: { type: "displayed", id } }); return; }
    throw new Error("XMPP session is not ready");
  }

  private async compatSendReaction(xmpp: CompatXmpp, to: string, type: "chat" | "groupchat", id: string, emojis: string[]) {
    if (xmpp.send_reaction) return xmpp.send_reaction(to, type, id, emojis);
    if (xmpp.sendMessage) { xmpp.sendMessage({ id: crypto.randomUUID(), to, type, reactions: { id, items: emojis } }); return; }
    throw new Error("XMPP session is not ready");
  }

  private async compatSendRetraction(xmpp: CompatXmpp, to: string, type: "chat" | "groupchat", id: string) {
    if (xmpp.send_retraction) return xmpp.send_retraction(to, type, id);
    if (xmpp.sendMessage) { xmpp.sendMessage({ id: crypto.randomUUID(), to, type, retract: { id } }); return; }
    throw new Error("XMPP session is not ready");
  }

  private async compatSendModeration(xmpp: CompatXmpp, roomJid: string, id: string, reason?: string) {
    if (xmpp.send_moderation) return xmpp.send_moderation(roomJid, "groupchat", id, reason);
    if (xmpp.sendMessage) { xmpp.sendMessage({ id: crypto.randomUUID(), to: roomJid, type: "groupchat", applyTo: { id, moderated: { retract: true, ...(reason ? { reason } : {}) } } }); return; }
    throw new Error("XMPP session is not ready");
  }

  private async compatSendCorrection(xmpp: CompatXmpp, to: string, type: "chat" | "groupchat", body: string, replacesId: string, opts?: SendGroupMessageOptions | SendDirectMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup } = encodeBodyForSend(body, opts?.replyTo, opts?.markup);
    const wasmOpts = buildWasmSendOptions({ ...(opts ?? {}), markup: rebasedMarkup }, replyFallbackLength);
    if (xmpp.send_correction) return await xmpp.send_correction(to, type, effectiveBody, replacesId, wasmOpts) as string;
    if (xmpp.sendMessage) {
      xmpp.sendMessage({ id: wasmOpts.stanza_id, to, type, body: effectiveBody, replace: replacesId });
      return wasmOpts.stanza_id ?? null;
    }
    throw new Error("XMPP session is not ready");
  }

  private async requireConnectedXmpp(): Promise<CompatXmpp> {
    await this.connect();
    if (!this.xmpp || !this.connected || this.destroying) throw new Error("XMPP session is not ready");
    return this.xmpp;
  }

  private async requireJoinedRoom(spaceId: string, channelId: string): Promise<{ xmpp: CompatXmpp; roomJid: string }> {
    const roomJid = this.roomJidForChannel(channelId);
    await this.switchRoom(spaceId, channelId);
    if (!this.xmpp || !this.connected || this.destroying || this.currentRoom !== roomJid) throw new Error(`Room is not ready: ${roomJid}`);
    return { xmpp: this.xmpp, roomJid };
  }

  private async flushQueuedDirectMessages() {
    if (this.directQueueFlushPromise) return this.directQueueFlushPromise;
    if (!this.canUseConnectedSession() || !this.xmpp) return;
    const promise = (async () => {
      const entries = listQueuedMessages(this.queueScope).filter((entry): entry is PersistedQueuedDmMessage => entry.kind === "dm");
      for (const entry of entries) {
        if (!this.canUseConnectedSession() || !this.xmpp) break;
        if (this.inflightQueuedIds.has(entry.id)) continue;
        this.queuedMessageStatusHandler?.(entry.id, "sending");
        const messageId = await this.compatSendDirectMessage(this.xmpp, barePeerJid(entry.peerJid), entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), id: entry.id });
        if (messageId) { this.inflightQueuedIds.add(entry.id); this.recordPendingSend(entry.id, "dm"); }
      }
    })();
    this.directQueueFlushPromise = promise.finally(() => { if (this.directQueueFlushPromise === promise) this.directQueueFlushPromise = null; });
    return this.directQueueFlushPromise;
  }

  private async flushQueuedRoomMessages(roomJid: string) {
    const inflight = this.roomQueueFlushes.get(roomJid);
    if (inflight) return inflight;
    if (!this.roomIsReady(roomJid) || !this.xmpp) return;
    const promise = (async () => {
      const entries = listQueuedRoomMessages(this.queueScope, roomJid);
      for (const entry of entries) {
        if (!this.roomIsReady(roomJid) || !this.xmpp) break;
        if (this.inflightQueuedIds.has(entry.id)) continue;
        this.queuedMessageStatusHandler?.(entry.id, "sending");
        const messageId = await this.compatSendGroupMessage(this.xmpp, roomJid, entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), mentionJidsByNick: { ...(entry.mentionJidsByNick ?? {}), ...this.roomMemberJids }, ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), ...(entry.threadCreate ? { threadCreate: entry.threadCreate } : {}), ...(entry.threadReply ? { threadReply: entry.threadReply } : {}), id: entry.id });
        if (messageId) { this.inflightQueuedIds.add(entry.id); this.recordPendingSend(entry.id, "room"); }
      }
    })();
    this.roomQueueFlushes.set(roomJid, promise.finally(() => { if (this.roomQueueFlushes.get(roomJid) === promise) this.roomQueueFlushes.delete(roomJid); }));
    return this.roomQueueFlushes.get(roomJid);
  }

  async sendGroupMessage(spaceId: string, channelId: string, body: string, opts: SendGroupMessageOptions = {}): Promise<OutboundSendResult | null> {
    const hasFiles = !!opts.files?.length;
    const hasThreadMetadata = !!opts.threadId?.trim();
    const hasForumMetadata = !!opts.threadCreate?.title?.trim() || !!opts.threadReply?.threadId?.trim();
    if (!body.trim() && !hasFiles && !hasThreadMetadata && !hasForumMetadata) return null;
    const roomJid = this.roomJidForChannel(channelId);
    if (this.roomIsReady(roomJid) && this.xmpp) {
      const id = await this.compatSendGroupMessage(this.xmpp, roomJid, body, { ...opts, mentionJidsByNick: { ...(opts.mentionJidsByNick ?? {}), ...this.roomMemberJids } });
      this.recordPendingSend(id, "room");
      return { id, state: "sending" };
    }
    const queued = this.queueRoomMessage(roomJid, body, opts);
    void this.connect().then(() => this.switchRoom(spaceId, channelId)).then(() => this.flushQueuedRoomMessages(roomJid)).catch(() => undefined);
    return queued;
  }

  async sendDirectMessage(peerJid: string, body: string, opts: SendDirectMessageOptions = {}): Promise<OutboundSendResult | null> {
    if (!body.trim() && !opts.files?.length) return null;
    const normalizedPeerJid = barePeerJid(peerJid);
    if (this.canUseConnectedSession() && this.xmpp) {
      const id = await this.compatSendDirectMessage(this.xmpp, normalizedPeerJid, body, opts);
      this.recordPendingSend(id, "dm");
      return { id, state: "sending" };
    }
    return this.queueDirectMessage(normalizedPeerJid, body, opts);
  }

  async sendChatState(spaceId: string, channelId: string, state: ChatStateType) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendChatState(xmpp, roomJid, "groupchat", state); }
  async sendDisplayed(spaceId: string, channelId: string, messageId: string) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendDisplayed(xmpp, roomJid, "groupchat", messageId); }
  async sendReaction(spaceId: string, channelId: string, messageId: string, emojis: string[]) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendReaction(xmpp, roomJid, "groupchat", messageId, emojis); }
  async sendRetraction(spaceId: string, channelId: string, retractsId: string) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendRetraction(xmpp, roomJid, "groupchat", retractsId); }
  async sendModeration(spaceId: string, channelId: string, targetId: string, reason?: string) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendModeration(xmpp, roomJid, targetId, reason); }
  async sendCorrection(spaceId: string, channelId: string, body: string, replacesId: string, markup?: SendGroupMessageOptions["markup"], references?: SendGroupMessageOptions["references"]): Promise<string | null> { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); return await this.compatSendCorrection(xmpp, roomJid, "groupchat", body, replacesId, { markup, references }); }
  async sendDmChatState(peerJid: string, state: ChatStateType): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendChatState(xmpp, barePeerJid(peerJid), "chat", state); }
  async sendDmDisplayed(peerJid: string, messageId: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendDisplayed(xmpp, barePeerJid(peerJid), "chat", messageId); }
  async sendDmRetraction(peerJid: string, messageId: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendRetraction(xmpp, barePeerJid(peerJid), "chat", messageId); }
  async sendDmCorrection(peerJid: string, body: string, replacesId: string, markup?: SendDirectMessageOptions["markup"], references?: SendDirectMessageOptions["references"]): Promise<string | null> { const xmpp = await this.requireConnectedXmpp(); return await this.compatSendCorrection(xmpp, barePeerJid(peerJid), "chat", body, replacesId, { markup, references }); }
  async sendDmReaction(peerJid: string, messageId: string, emojis: string[]): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendReaction(xmpp, barePeerJid(peerJid), "chat", messageId, emojis); }

  private async resolveUploadService(): Promise<string> {
    if (this.uploadServiceJid) return this.uploadServiceJid;
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.discover_upload_service) throw new Error("XMPP not connected");
    const jid = await discoverUploadService(xmpp as WasmClient);
    if (!jid) throw new Error(`File upload service not available (domain: ${jidDomain(this.session.jid)})`);
    this.uploadServiceJid = jid;
    return jid;
  }

  async uploadAttachments(files: ReadonlyArray<File | Blob>, onProgress?: (overall: UploadProgress, index: number) => void): Promise<OutboundFileAttachment[]> {
    const xmpp = await this.requireConnectedXmpp();
    const uploadDomain = await this.resolveUploadService();
    const preparedUploads = await Promise.all(files.map((file) => prepareEncryptedAttachmentUpload(file)));
    const totals = preparedUploads.map((prepared) => prepared.uploadFile.size);
    const loaded = files.map(() => 0);
    const grandTotal = totals.reduce((a, b) => a + b, 0);
    const results: OutboundFileAttachment[] = [];
    for (const [index, prepared] of preparedUploads.entries()) {
      const result = await uploadFile(xmpp as WasmClient, prepared.uploadFile, uploadDomain, (progress) => {
        loaded[index] = progress.loaded;
        onProgress?.({ loaded: loaded.reduce((a, b) => a + b, 0), total: grandTotal }, index);
      });
      results.push({ url: result.getUrl, name: prepared.originalName, mediaType: prepared.originalMediaType, size: prepared.originalSize, disposition: inferredFileDisposition(prepared.originalMediaType, prepared.originalName), encrypted: { ...prepared.encrypted, sources: [result.getUrl] } });
    }
    return results;
  }

  async invokeExtensionLaunch(launch: any): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return invokeExtensionLaunch(xmpp as WasmClient, this.session.jid, launch); }
  async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> { const xmpp = await this.requireConnectedXmpp(); return discoverExtensionCommands(xmpp as WasmClient, this.session.jid); }
  async invokeExtensionCommand(command: DiscoveredExtensionCommand): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return invokeExtensionCommand(xmpp as WasmClient, this.session.jid, command); }
  async submitExtensionCommandForm(command: DiscoveredExtensionCommand, sessionId: string, fields: ExtensionCommandFormField[], action?: ExtensionCommandAction): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return submitExtensionCommandForm(xmpp as WasmClient, command, sessionId, fields, action); }

  async enablePushNotifications(opts: { serviceJid: string; node?: string; endpoint: string; p256dh: string; auth: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.send_raw_iq) return false;
    const node = opts.node ?? "web-push";
    const xml = `<iq type="set" id="${crypto.randomUUID()}"><enable xmlns="urn:xmpp:push:0" jid="${opts.serviceJid}" node="${node}"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#publish-options</value></field><field var="service"><value>${opts.endpoint}</value></field><field var="device-token"><value>${opts.auth}</value></field><field var="device-key"><value>${opts.p256dh}</value></field></x></enable></iq>`;
    try { await xmpp.send_raw_iq(xml); return true; } catch { return false; }
  }

  async disablePushNotifications(opts: { serviceJid: string; node?: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.disable_push_notifications) return false;
    try { await xmpp.disable_push_notifications(opts.serviceJid, opts.node ?? "web-push"); return true; } catch { return false; }
  }

  async fetchInbox(opts: FetchInboxOptions = {}): Promise<InboxResult> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.fetch_inbox?.({ ...(typeof opts.since === "number" ? { since: opts.since } : {}), ...(opts.onlyUnread ? { only_unread: true } : {}), ...(opts.room ? { room: opts.room } : {}), ...(opts.threads ? { threads: true } : {}) }) as { total_unread: number; conversations: WasmInboxConversation[] };
    return { totalUnread: result?.total_unread ?? 0, conversations: (result?.conversations ?? []).map(inboxEntryFromWasm) };
  }

  async markInboxRead(partnerJid: string, threadId?: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.mark_inbox_read?.(barePeerJid(partnerJid), threadId ?? null); }
  async fetchThreadInbox(roomJid: string): Promise<InboxResult> { return this.fetchInbox({ room: roomJid, threads: true }); }
  async publishMood(mood: MoodPublication): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.publish_mood?.({ kind: mood.kind, ...(mood.text ? { text: mood.text } : {}) }); }
  async retractMood(): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.retract_mood?.(); }
  async publishActivity(activity: ActivityPublication): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.publish_activity?.({ general: activity.general, ...(activity.specific ? { specific: activity.specific } : {}), ...(activity.text ? { text: activity.text } : {}) }); }
  async retractActivity(): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.retract_activity?.(); }
  async publishTune(tune: TunePublication): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.publish_tune?.(tune); }
  async retractTune(): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.retract_tune?.(); }
  async fetchUserPepProfile(jid: string): Promise<UserPepProfile> {
    const xmpp = await this.requireConnectedXmpp();
    const profile = await xmpp.fetch_user_pep_profile?.(jid) as WasmPepProfile | null;
    return { mood: profile?.mood ? { kind: profile.mood.kind as any, ...(profile.mood.text ? { text: profile.mood.text } : {}) } : null, activity: profile?.activity ? { general: profile.activity.general as any, ...(profile.activity.specific ? { specific: profile.activity.specific } : {}), ...(profile.activity.text ? { text: profile.activity.text } : {}) } : null, tune: profile?.tune ? { ...profile.tune } : null };
  }

  private roomMamPageToMessages(page: WasmMamPage): MamHistoryPage<LiveRoomMessage> {
    return { messages: page.messages.map(roomMessageFromArchived).filter((message): message is LiveRoomMessage => !!message), ...(page.first_id ? { firstArchiveId: page.first_id } : {}), ...(page.last_id ? { lastArchiveId: page.last_id } : {}), complete: page.is_complete };
  }
  private dmMamPageToMessages(page: WasmMamPage): MamHistoryPage<LiveDmMessage> {
    const selfBare = barePeerJid(this.session.jid);
    return { messages: page.messages.map((message) => dmMessageFromArchived(message, selfBare)).filter((message): message is LiveDmMessage => !!message), ...(page.first_id ? { firstArchiveId: page.first_id } : {}), ...(page.last_id ? { lastArchiveId: page.last_id } : {}), complete: page.is_complete };
  }

  async queryMam(spaceId: string, channelId: string, max = 50): Promise<LiveRoomMessage[]> { const page = await this.queryMamPage(spaceId, channelId, max, { type: "latest" }); return page.messages; }
  async queryMamPage(spaceId: string, channelId: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_page?.(this.roomJidForChannel(channelId), max, pageParam) as WasmMamPage; return page ? this.roomMamPageToMessages(page) : { messages: [], complete: true };
  }
  async queryMamByThread(spaceId: string, channelId: string, threadId: string, max = 100): Promise<LiveRoomMessage[]> {
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_by_thread?.(this.roomJidForChannel(channelId), threadId, max, null) as WasmMamPage; return page ? this.roomMamPageToMessages(page).messages : [];
  }
  async queryMamThreadPage(spaceId: string, channelId: string, threadId: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    if (!threadId) return { messages: [], complete: true };
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_by_thread?.(this.roomJidForChannel(channelId), threadId, max, pageParam.type === "before" ? pageParam.before : null) as WasmMamPage; return page ? this.roomMamPageToMessages(page) : { messages: [], complete: true };
  }
  async searchMessages(_spaceId: string, channelId: string, query: string, max = 20): Promise<MessageSearchResult[]> {
    if (!query.trim()) return [];
    const xmpp = await this.requireConnectedXmpp();
    const page = await xmpp.search_room_history?.(this.roomJidForChannel(channelId), query, max) as WasmMamPage;
    const parsed = page ? this.roomMamPageToMessages(page).messages : [];
    return parsed.filter((message) => !!message.body).map((message, index) => ({ id: message.id, ...(page?.messages[index]?.mam_id ? { archiveId: page.messages[index].mam_id } : {}), nick: message.nick, body: message.body, createdAt: message.createdAt, ...(message.threadId ? { threadId: message.threadId } : {}), ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}), roomJid: message.roomJid }));
  }
  async queryPersonalMam(peerJid: string, max = 100): Promise<LiveDmMessage[]> { const page = await this.queryPersonalMamPage(peerJid, max, { type: "latest" }); return page.messages; }
  async queryPersonalMamPage(peerJid: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveDmMessage>> { const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_dm_history_page?.(barePeerJid(peerJid), max, pageParam) as WasmMamPage; return page ? this.dmMamPageToMessages(page) : { messages: [], complete: true }; }
  async searchDmMessages(peerJid: string, query: string, max = 20): Promise<MessageSearchResult[]> {
    if (!query.trim()) return [];
    const xmpp = await this.requireConnectedXmpp();
    const page = await xmpp.search_dm_history?.(barePeerJid(peerJid), query, max) as WasmMamPage;
    const parsed = page ? this.dmMamPageToMessages(page).messages : [];
    return parsed.filter((message) => !!message.body).map((message, index) => ({ id: message.id, ...(page?.messages[index]?.mam_id ? { archiveId: page.messages[index].mam_id } : {}), nick: message.nick, body: message.body, createdAt: message.createdAt, ...(message.threadId ? { threadId: message.threadId } : {}), ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}), peerJid: message.peerJid }));
  }
  async subscribeToPeerPresence(peerJid: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.subscribe_to_presence?.(barePeerJid(peerJid)); }
  async listRosterContacts(): Promise<RosterContact[]> { const xmpp = await this.requireConnectedXmpp(); const roster = await xmpp.list_roster_contacts?.() as WasmRosterContact[]; return (roster ?? []).map((item) => { const jid = barePeerJid(item.jid); return { jid, name: item.name, username: item.name?.trim() || jid.split("@")[0] || jid, subscription: (item.subscription ?? "none") as RosterContact["subscription"], groups: item.groups ?? [] }; }); }
  async getServerVersion(): Promise<WasmServerVersion | null> { const xmpp = await this.requireConnectedXmpp(); return await xmpp.get_server_version?.() as WasmServerVersion | null; }
  async discoverSpaceChannels(): Promise<any[]> { const xmpp = await this.requireConnectedXmpp(); return discoverChannels(xmpp as WasmClient, this.session.jid); }
  async discoverTopology(): Promise<DiscoveredTopology> { const xmpp = await this.requireConnectedXmpp(); const topology = await discoverTopology(xmpp as WasmClient, this.session.jid); this.discoveredRoomJids = new Map(topology.rooms.flatMap((room) => room.jid ? [[room.id, room.jid] as const] : [])); return topology; }
  async listRoomMembers(channelId: string, options?: ListRoomMembersOptions): Promise<MemberSummary[]> {
    const xmpp = await this.requireConnectedXmpp(); const roomJid = options?.roomJid ?? this.roomJidForChannel(channelId);
    const listMembers = xmpp.list_room_members
      ? async (affiliation: "owner" | "admin" | "member" | "outcast") => await xmpp.list_room_members?.(roomJid, affiliation) as WasmRoomMember[]
      : (xmpp as CompatXmpp & { getRoomMembers?: (room: string, opts: { affiliation: "owner" | "admin" | "member" | "outcast" }) => Promise<{ muc?: { users?: Array<{ jid?: string }> } }> }).getRoomMembers
        ? async (affiliation: "owner" | "admin" | "member" | "outcast") => (await (xmpp as CompatXmpp & { getRoomMembers: (room: string, opts: { affiliation: "owner" | "admin" | "member" | "outcast" }) => Promise<{ muc?: { users?: Array<{ jid?: string }> } }> }).getRoomMembers(roomJid, { affiliation }))?.muc?.users?.map((user) => ({ jid: user.jid })) ?? []
        : null;
    if (!listMembers) { this.emitError({ kind: "member-query", recoverable: false, detail: "missing getRoomMembers" }); throw new Error("missing getRoomMembers"); }
    const affiliations = ["owner", "admin", "member", "outcast"] as const; const members: MemberSummary[] = []; const failedAffiliations: string[] = [];
    for (const affiliation of affiliations) {
      try { const result = await listMembers(affiliation); for (const item of result ?? []) { if (!item.jid) continue; members.push({ jid: item.jid, username: item.jid.split("@")[0] ?? item.jid, avatar_url: null, role: affiliation, joined_at: "" }); } } catch (error: any) { failedAffiliations.push(affiliation); const condition = error?.condition ?? error?.error?.condition; const detail = condition === "forbidden" ? `forbidden affiliation query — ${roomJid}` : condition === "service-unavailable" ? `unsupported member query — ${roomJid}` : `affiliation query failed for ${affiliation} — ${roomJid}; reconstructed room JID may not match`; this.emitError({ kind: "member-query", recoverable: true, detail, cause: error, condition }); }
    }
    if (members.length === 0 && failedAffiliations.length > 0) throw new Error("refusing to show Members 0");
    return members;
  }
  async setRoomAffiliation(channelId: string, jid: string, affiliation: MemberSummary["role"]): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.set_room_affiliation?.(this.roomJidForChannel(channelId), jid, affiliation === "none" ? "none" : affiliation); }
  async searchUsers(query: string): Promise<UserSearchResult[]> { if (!query.trim()) return []; const xmpp = await this.requireConnectedXmpp(); const users = await xmpp.search_users?.(query) as WasmUserSearchResult[]; return (users ?? []).map((user) => ({ id: user.jid, jid: user.jid, username: user.username ?? user.nick ?? user.jid.split("@")[0] ?? user.jid, display_name: user.display_name ?? user.name ?? null, avatar_url: null })); }
  async fetchUserAvatar(jid: string): Promise<string | null> {
    const xmpp = await this.requireConnectedXmpp();
    const bareJid = barePeerJid(jid);
    if (xmpp.request_avatar) {
      const avatar = await xmpp.request_avatar(jid) as WasmAvatar | null;
      if (avatar) return avatarDataUrl(avatar.data, avatar.mime_type);
    }
    const legacy = xmpp as CompatXmpp & {
      getItems?: (jid: string, node: string) => Promise<{ items?: Array<{ content?: { versions?: Array<{ id?: string; mediaType?: string }> } }> }>;
      getAvatar?: (jid: string, id: string) => Promise<{ content?: { data?: Uint8Array } }>;
      getVCard?: (jid: string) => Promise<{ records?: Array<{ type?: string; mediaType?: string; data?: Uint8Array; url?: string }> }>;
    };
    try {
      const metadata = await legacy.getItems?.(bareJid, "urn:xmpp:avatar:metadata");
      const version = metadata?.items?.[0]?.content?.versions?.[0];
      if (version?.id) {
        const avatar = await legacy.getAvatar?.(bareJid, version.id);
        const data = avatar?.content?.data;
        if (data) return avatarDataUrl(data, version.mediaType ?? "image/png");
      }
    } catch {}
    const vcard = await legacy.getVCard?.(bareJid);
    const photo = vcard?.records?.find((record) => record.type === "photo");
    if (photo?.data) return avatarDataUrl(photo.data, photo.mediaType ?? "image/png");
    if (photo?.url) return photo.url;
    return null;
  }
  get agent(): CompatXmpp | null { return this.xmpp; }

  private startSelfPing() { this.stopSelfPing(); this.selfPingTimer = setInterval(() => { void this.doSelfPing(); }, 60000); }
  private stopSelfPing() { if (this.selfPingTimer) { clearInterval(this.selfPingTimer); this.selfPingTimer = null; } }
  private async doSelfPing() { if (!this.xmpp?.send_raw_iq || !this.currentRoom) return; try { await this.xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${this.currentRoom}/${this.session.username}"><ping xmlns="urn:ietf:params:xml:ns:xmpp-ping"/></iq>`); } catch { this.roomDisconnectHandler?.(); } }
  private handleMessageAck(id: string) { const wasQueued = this.inflightQueuedIds.delete(id); if (wasQueued) removeQueuedMessage(this.queueScope, id); this.messageAckHandler?.(id); const pending = this.pendingSendAt.get(id); if (pending) { this.pendingSendAt.delete(id); this.fireHook(this.messageAckHooks, id, { kind: pending.kind, latencyMs: performance.now() - pending.at }); } if (wasQueued) this.emitQueueDepth(); }
  private handleMessageFailed(id: string) { const wasQueued = this.inflightQueuedIds.delete(id); this.messageDeliveryFailureHandler?.(id); const pending = this.pendingSendAt.get(id); if (pending) { this.pendingSendAt.delete(id); this.fireHook(this.messageFailHooks, id, { kind: pending.kind }); } if (wasQueued) this.emitQueueDepth(); }
  private handleSessionReady(xmpp: CompatXmpp, lifecycle: SessionLifecycleEvent) {
    if (this.xmpp !== xmpp) return;
    this.connected = true; this.reconnectAttempt = 0;
    this.emitStatus({ state: "online", detail: countQueuedMessages(this.queueScope) > 0 ? lifecycle.type === "fresh" ? "Reconnected — replaying queued messages" : "Connection resumed — replaying queued messages" : lifecycle.type === "fresh" ? "Connection ready" : "Connection resumed" });
    if (lifecycle.type === "fresh") { this.inflightQueuedIds.clear(); void this.enableCarbons(xmpp); void this.refreshRosterPresenceSubscriptions(xmpp); }
    else {
      const catchupTargets = this.catchup.onSessionStarted();
      if (catchupTargets.length > 0 && (xmpp as CompatXmpp & { searchHistory?: (jid: string, opts: { paging: { max: number } }) => Promise<unknown> }).searchHistory) {
        void (xmpp as CompatXmpp & { searchHistory: (jid: string, opts: { paging: { max: number } }) => Promise<unknown> }).searchHistory(barePeerJid(this.session.jid), { paging: { max: 200 } });
      }
    }
    this.emitSessionLifecycle(lifecycle); void this.flushQueuedDirectMessages(); if (this.currentRoom) void this.flushQueuedRoomMessages(this.currentRoom);
    if (this.onceConnected) { const done = this.onceConnected; this.onceConnected = null; this.onceConnectFailed = null; done(); }
  }
  private handleDisconnected(xmpp: CompatXmpp, error?: Error) {
    if (this.xmpp !== xmpp) return;
    this.connected = false; this.stopSelfPing(); this.xmpp = null;
    if (this.destroying) { this.emitStatus({ state: "offline", detail: error?.message ?? "Disconnected" }); return; }
    this.emitStatus({ state: "reconnecting", detail: countQueuedMessages(this.queueScope) > 0 ? "Connection lost — queued messages will send when reconnected" : (error?.message ?? "Connection lost, reconnecting...") });
    if (this.onceConnectFailed) { const fail = this.onceConnectFailed; this.onceConnected = null; this.onceConnectFailed = null; fail(error ?? new Error("XMPP connection failed")); }
    this.scheduleReconnect();
  }
  private handlePresence(presence: WasmPresence) {
    const from = presence.from ?? "";
    if ((presence as any).muc_jid !== undefined) {
      const [room, nick = ""] = from.split("/"); const waiter = this.roomJoinWaiters.get(from); if (waiter && (presence as any).muc_jid) waiter.resolve(); if (!room) return; if (!nick && presence.vcard_avatar) this.roomAvatarHandler?.(room, presence.vcard_avatar); if (room !== this.currentRoom || !nick) return;
      if (presence.presence_type === "unavailable") { delete this.roomHats[nick]; this.hatsHandler?.({ ...this.roomHats }); this.roomPresence[nick] = "offline"; this.presenceHandler?.({ ...this.roomPresence }); delete this.roomMemberJids[nick]; this.lastSeenHandler?.(nick, Date.now()); return; }
      this.roomHats[nick] = mergeOccupantHats(roleHatsForOccupant((presence as any).muc_affiliation, (presence as any).muc_role), ((presence as any).hats ?? []).map((hat: any) => ({ uri: hat.uri, title: hat.title })));
      this.hatsHandler?.({ ...this.roomHats }); this.roomPresence[nick] = parsePresenceShow(presence.show); this.presenceHandler?.({ ...this.roomPresence }); if ((presence as any).muc_jid) { const bare = barePeerJid((presence as any).muc_jid); this.roomMemberJids[nick] = bare; this.memberJidHandler?.(nick, bare); } return;
    }
    const bare = barePeerJid(from); if (!bare) return; if (presence.presence_type === "subscribe") return; this.presenceUpdateHandler?.({ bareJid: bare, show: mapPresenceShow(presence), ...(presence.status ? { status: presence.status } : {}) });
  }
  private handleMessage(message: WasmMessage & { carbon?: { sent?: boolean; received?: boolean }; inboxPush?: InboxEntry; _fromCarbon?: boolean }) {
    if (message.inboxPush) { this.inboxPushHandler?.(message.inboxPush); return; }
    if (message.carbon?.sent || message.carbon?.received) return;
    if (message.id && this.carbonDedupIds.has(message.id) && !message._fromCarbon) {
      this.carbonDedupIds.delete(message.id);
      return;
    }
    if (message.chat_state && !message.body) {
      if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; if (roomJid === this.currentRoom && nick !== this.session.username) this.chatStateHandler?.({ roomJid, nick, state: message.chat_state as ChatStateType }); }
      else this.dmChatStateHandler?.({ peerJid: barePeerJid(message.from ?? message.to ?? ""), state: message.chat_state as ChatStateType });
      return;
    }
    if (message.displayed_marker_id) { if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; this.displayedHandler?.({ roomJid, nick, messageId: message.displayed_marker_id }); } else this.dmDisplayedHandler?.({ peerJid: barePeerJid(message.from ?? message.to ?? ""), messageId: message.displayed_marker_id }); return; }
    if (message.reaction_target_id) { if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; this.reactionHandler?.({ roomJid, nick, messageId: message.reaction_target_id, emojis: message.reaction_emojis }); } else this.dmReactionHandler?.({ peerJid: barePeerJid(message.from ?? message.to ?? ""), messageId: message.reaction_target_id, emojis: message.reaction_emojis }); return; }
    if (message.is_muc) {
      const converted = roomMessageFromArchived({ ...message, mam_id: message.id ?? crypto.randomUUID() } as WasmArchivedMessage);
      if (!converted) return;
      if (converted.roomJid !== this.currentRoom && converted.body) { const activity: RoomActivityEvent = { roomJid: converted.roomJid, nick: converted.nick, body: converted.body }; if (converted.mentions) (activity as any).mentions = converted.mentions; if ((converted as any).broadcastMention) (activity as any).broadcastMention = (converted as any).broadcastMention; this.activityHandler?.(activity); return; }
      this.messageHandler?.(converted); return;
    }
    const converted = dmMessageFromArchived({ ...message, mam_id: message.id ?? crypto.randomUUID() } as WasmArchivedMessage, barePeerJid(this.session.jid));
    if (converted) this.directMessageHandler?.(converted);
  }
  private wireEvents(xmpp: CompatXmpp & { enableKeepAlive?: (opts: { interval: number; timeout: number }) => void; disableKeepAlive?: () => void }) {
    xmpp.set_on_connected?.(() => { if (this.xmpp !== xmpp) return; void this.enableCarbons(xmpp); });
    xmpp.set_on_session_lifecycle?.((event: string) => { if (event === "resumed") this.handleSessionReady(xmpp, { type: "resumed" }); else this.handleSessionReady(xmpp, { type: "fresh" }); });
    xmpp.set_on_disconnected?.(() => this.handleDisconnected(xmpp));
    xmpp.set_on_error?.((detail: string) => this.emitError({ kind: "stream", recoverable: !this.destroying, detail }));
    xmpp.set_on_message?.((message: WasmMessage) => this.handleMessage(message));
    xmpp.set_on_presence?.((presence: WasmPresence) => this.handlePresence(presence));
    xmpp.set_on_message_delivery_acked?.((id: string) => this.handleMessageAck(id));
    xmpp.set_on_message_delivery_failed?.((id: string) => this.handleMessageFailed(id));
    xmpp.on?.("session:started", () => { xmpp.disableKeepAlive?.(); xmpp.enableKeepAlive?.({ interval: 30, timeout: 15 }); this.handleSessionReady(xmpp, { type: "fresh" }); });
    xmpp.on?.("stream:management:resumed", () => this.handleSessionReady(xmpp, { type: "resumed" }));
    xmpp.on?.("disconnected", (error?: Error) => { xmpp.disableKeepAlive?.(); this.handleDisconnected(xmpp, error); });
    xmpp.on?.("message:acked", (msg: { id?: string }) => { if (msg?.id) this.handleMessageAck(msg.id); });
    xmpp.on?.("message:failed", (msg: { id?: string }) => { if (msg?.id) this.handleMessageFailed(msg.id); });
    xmpp.on?.("message", (message: WasmMessage) => this.handleMessage(message));
    xmpp.on?.("carbon:sent", (event: { carbon?: { forward?: { message?: WasmMessage } } }) => { const forwarded = event.carbon?.forward?.message; if (forwarded?.id) this.carbonDedupIds.add(forwarded.id); if (forwarded) this.handleMessage({ ...forwarded, _fromCarbon: true }); });
    xmpp.on?.("carbon:received", (event: { carbon?: { forward?: { message?: WasmMessage } } }) => { const forwarded = event.carbon?.forward?.message; if (forwarded?.id) this.carbonDedupIds.add(forwarded.id); if (forwarded) this.handleMessage({ ...forwarded, _fromCarbon: true }); });
    xmpp.on?.("presence", (presence: WasmPresence) => this.handlePresence(presence));
  }
}

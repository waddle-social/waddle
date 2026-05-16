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
  MamThreadPageParam,
  MessageSearchResult,
  PresenceUpdateEvent,
  ReactionEvent,
  RoomActivityEvent,
  RoomHats,
  RoomPresence,
  RosterContact,
  SessionLifecycleEvent,
  XmppErrorEvent,
  XmppStatusSnapshot,
} from "./types";
import { mergeOccupantHats, roleHatsForOccupant } from "./occupant-badges";
import { prepareEncryptedAttachmentUpload } from "./encrypted-attachments";

import { discoverChannels, discoverTopology } from "./discovery";
import { discoverUploadService, uploadFile, type UploadProgress } from "./file-upload";
import { ReconnectCatchup } from "./reconnect-catchup";
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
  type OutboundFileAttachment,
  type SendDirectMessageOptions,
  type SendGroupMessageOptions,
} from "./send-types";
import {
  avatarDataUrl,
  buildWasmSendOptions,
  dmMessageFromArchived,
  encodeBodyForSend,
  inboxEntryFromWasm,
  mapPresenceShow,
  parsePresenceShow,
  roomMessageFromArchived,
} from "./wasm-message-codecs";
import type {
  WasmArchivedMessage,
  WasmAvatar,
  WasmInboxResult,
  WasmMamPage,
  WasmMessage,
  WasmPepProfile,
  WasmPresence,
  WasmRoomMember,
  WasmRosterContact,
  WasmServerVersion,
  WasmUserSearchResult,
} from "./wasm-types";

export { dmMessageFromArchived, roomMessageFromArchived } from "./wasm-message-codecs";

function isRoomActivityMessage(message: LiveRoomMessage): boolean {
  return !!message.body && !message.replacesId && !message.retractsId;
}

function roomActivityEventFromMessage(message: LiveRoomMessage): RoomActivityEvent {
  const activity: RoomActivityEvent = { roomJid: message.roomJid, nick: message.nick, body: message.body };
  if (message.mentions) activity.mentions = message.mentions;
  if (message.broadcastMention) activity.broadcastMention = message.broadcastMention;
  return activity;
}

function isMamPageComplete(page: WasmMamPage | null | undefined): boolean {
  const compat = page as (WasmMamPage & { complete?: boolean }) | null | undefined;
  return !!(compat?.is_complete ?? compat?.complete);
}

function pageLastArchiveId(page: WasmMamPage | null | undefined): string | undefined {
  const compat = page as (WasmMamPage & { lastArchiveId?: string }) | null | undefined;
  return compat?.last_id ?? compat?.lastArchiveId;
}

function pageFirstArchiveId(page: WasmMamPage | null | undefined): string | undefined {
  const compat = page as (WasmMamPage & { firstArchiveId?: string }) | null | undefined;
  return compat?.first_id ?? compat?.firstArchiveId;
}

function compareTimestamps(left: string, right: string): number {
  const leftMs = Date.parse(left);
  const rightMs = Date.parse(right);
  if (Number.isFinite(leftMs) && Number.isFinite(rightMs)) {
    return leftMs === rightMs ? 0 : leftMs < rightMs ? -1 : 1;
  }
  return left.localeCompare(right);
}

function pageCrossesSince(page: WasmMamPage | null | undefined, since: string): boolean {
  return (page?.messages ?? []).some((message) => typeof message.timestamp === "string" && compareTimestamps(message.timestamp, since) < 0);
}

function messageSeenIds(message: Pick<LiveDmMessage | LiveRoomMessage, "id" | "wireIds">): string[] {
  return Array.from(new Set([message.id, ...(message.wireIds ?? [])].filter(Boolean)));
}

function rawMessageSeenIds(message: WasmMessage): string[] {
  return Array.from(new Set([
    message.id,
    message.origin_id,
    message.stanza_id,
    ...(message.stanza_ids?.map((stanzaId) => stanzaId.id) ?? []),
  ].filter((value): value is string => !!value)));
}

function shouldSkipCatchupMessage(
  message: Pick<LiveDmMessage | LiveRoomMessage, "createdAt" | "id" | "wireIds">,
  since?: string,
  seenIds?: ReadonlyArray<string>,
): boolean {
  const seen = new Set(seenIds ?? []);
  if (seen.size > 0 && messageSeenIds(message).some((id) => seen.has(id))) return true;
  if (!since) return false;
  const order = compareTimestamps(message.createdAt, since);
  if (order < 0) return true;
  if (order > 0) return false;
  return false;
}

type WasmModule = typeof import("@waddle/xmpp-client-wasm");
type WasmClient = import("@waddle/xmpp-client-wasm").WaddleClient & {
  discover_extension_routes?: () => Promise<unknown>;
  fetch_extension_route_items?: (route: unknown, roomJid: string) => Promise<unknown>;
};

type CompatEmitter = {
  on?: (event: string, handler: (...args: any[]) => void) => void;
  off?: (event: string, handler: (...args: any[]) => void) => void;
};

type XmppClientInstance = Partial<WasmClient> & CompatEmitter & {
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
  get_resume_state?: () => XmppResumeState | null;
  get_resume_state_handle?: () => XmppResumeStateHandle | undefined;
};

type XmppResumeState = {
  previd: string;
  inboundH: number;
  outboundH: number;
};

type XmppResumeStateHandle = import("@waddle/xmpp-client-wasm").WaddleResumeState;

interface OutboundSendResult {
  id: string | null;
  state: "queued" | "sending";
}

let wasmModulePromise: Promise<WasmModule> | null = null;

function createXmppResource() {
  const randomId = globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
  return `web-${randomId}`;
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

export class RoomMemberListUnavailableError extends Error {
  constructor(message = "Member list is temporarily unavailable.") {
    super(message);
    this.name = "RoomMemberListUnavailableError";
  }
}

export class BrowserXmppClient {
  private session: WaddleSession;
  private get queueScope() { return barePeerJid(this.session.jid); }
  private readonly resource = createXmppResource();
  private messageHandler: ((message: LiveRoomMessage) => void) | null = null;
  private pinEventHandler: ((event: { roomJid: string; event: import("./wasm-types").WasmPinEvent }) => void) | null = null;
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
  private xmpp: XmppClientInstance | null = null;
  private connectPromise: Promise<void> | null = null;
  private connected = false;
  private destroying = false;
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
  private resumeState: XmppResumeState | null = null;
  private resumeStateHandle: XmppResumeStateHandle | null = null;
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
  readonly catchup = new ReconnectCatchup();

  constructor(session: WaddleSession) { this.session = session; }

  setMessageHandler(h: (message: LiveRoomMessage) => void) { this.messageHandler = h; }
  /** #414: receive `<pin-event/>` system messages from a room. */
  setPinEventHandler(h: (event: { roomJid: string; event: import("./wasm-types").WasmPinEvent }) => void) { this.pinEventHandler = h; }
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

  private clearResumeState() {
    this.resumeState = null;
    this.setResumeStateHandle(null);
  }

  private setResumeStateHandle(handle: XmppResumeStateHandle | null | undefined) {
    if (this.resumeStateHandle && this.resumeStateHandle !== handle) {
      this.disposeResumeStateHandle(this.resumeStateHandle);
    }
    this.resumeStateHandle = handle ?? null;
  }

  private disposeResumeStateHandle(handle: XmppResumeStateHandle) {
    try {
      handle.free();
    } catch {}
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

  private async enableCarbons(xmpp: XmppClientInstance & { enableCarbons?: () => Promise<void> }) {
    if (xmpp.enableCarbons) {
      try { await xmpp.enableCarbons(); } catch {}
      return;
    }
    if (!xmpp.send_raw_iq) return;
    try { await xmpp.send_raw_iq(`<iq type="set" id="${crypto.randomUUID()}"><enable xmlns="urn:xmpp:carbons:2"/></iq>`); } catch {}
  }

  private async doConnect(): Promise<void> {
    const mod = await loadWasmModule();
    const config = new mod.WaddleConfig(
      this.session.xmpp_websocket_url,
      this.session.jid,
      this.session.session_id,
      this.resource,
    );
    if (this.resumeStateHandle && typeof (config as any).with_resume_state_handle === "function") {
      const handle = this.resumeStateHandle;
      (config as any).with_resume_state_handle(handle);
      this.clearResumeState();
    } else if (this.resumeState) {
      (config as any).with_resume_state?.(
        this.resumeState.previd,
        this.resumeState.inboundH,
        this.resumeState.outboundH,
      );
      this.resumeState = null;
    }
    const xmpp = new mod.WaddleClient(config) as unknown as XmppClientInstance;
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
    this.clearResumeState();
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
        const detail = `Timed out waiting for self-presence in ${roomJid}`;
        this.emitError({ kind: "connect-timeout", recoverable: true, detail });
        reject(new Error("Channel presence did not finish syncing. Try again in a moment."));
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

  private async compatSendGroupMessage(xmpp: XmppClientInstance, roomJid: string, body: string, opts: SendGroupMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup, rebasedReferences } = encodeBodyForSend(body, opts.replyTo, opts.markup, opts.references);
    const wasmOpts = buildWasmSendOptions({ ...opts, markup: rebasedMarkup, references: rebasedReferences }, replyFallbackLength);
    if (xmpp.send_groupchat_message) return await xmpp.send_groupchat_message(roomJid, effectiveBody, wasmOpts) as string;
    throw new Error("XMPP session is not ready");
  }

  private async compatSendDirectMessage(xmpp: XmppClientInstance, peerJid: string, body: string, opts: SendDirectMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup, rebasedReferences } = encodeBodyForSend(body, opts.replyTo, opts.markup, opts.references);
    const wasmOpts = buildWasmSendOptions({ ...opts, markup: rebasedMarkup, references: rebasedReferences }, replyFallbackLength);
    if (xmpp.send_chat_message) return await xmpp.send_chat_message(peerJid, effectiveBody, wasmOpts) as string;
    throw new Error("XMPP session is not ready");
  }

  private async compatSendChatState(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", state: ChatStateType) {
    if (xmpp.send_chat_state) return xmpp.send_chat_state(to, type, state);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendDisplayed(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", id: string) {
    if (xmpp.send_displayed) return xmpp.send_displayed(to, type, id);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendReaction(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", id: string, emojis: string[]) {
    if (xmpp.send_reaction) return xmpp.send_reaction(to, type, id, emojis);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendRetraction(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", id: string) {
    if (xmpp.send_retraction) return xmpp.send_retraction(to, type, id);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendModeration(xmpp: XmppClientInstance, roomJid: string, id: string, reason?: string) {
    if (xmpp.send_moderation) return xmpp.send_moderation(roomJid, "groupchat", id, reason);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendCorrection(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", body: string, replacesId: string, opts?: SendGroupMessageOptions | SendDirectMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup, rebasedReferences } = encodeBodyForSend(body, opts?.replyTo, opts?.markup, opts?.references);
    const wasmOpts = buildWasmSendOptions({ ...(opts ?? {}), markup: rebasedMarkup, references: rebasedReferences }, replyFallbackLength);
    if (xmpp.send_correction) return await xmpp.send_correction(to, type, effectiveBody, replacesId, wasmOpts) as string;
    throw new Error("XMPP session is not ready");
  }

  private async requireConnectedXmpp(): Promise<XmppClientInstance> {
    await this.connect();
    if (!this.xmpp || !this.connected || this.destroying) throw new Error("XMPP session is not ready");
    return this.xmpp;
  }

  private async requireJoinedRoom(spaceId: string, channelId: string): Promise<{ xmpp: XmppClientInstance; roomJid: string }> {
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
  /** #414: pin a message in the room. Server gates on Owner/Admin
   * affiliation; non-admins receive a `<forbidden/>` error which
   * surfaces as a rejected Promise. */
  async pinMessage(spaceId: string, channelId: string, targetStanzaId: string) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    if (typeof xmpp.pin_message !== "function") throw new Error("pin_message not available in wasm client");
    await xmpp.pin_message(roomJid, targetStanzaId);
  }
  /** #414: unpin a message in the room. Same authorization rules. */
  async unpinMessage(spaceId: string, channelId: string, targetStanzaId: string) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    if (typeof xmpp.unpin_message !== "function") throw new Error("unpin_message not available in wasm client");
    await xmpp.unpin_message(roomJid, targetStanzaId);
  }
  /** #414: fetch the room's current pin list. Server requires the
   * caller to be a current room occupant. Returns entries in
   * pin-time-desc order. */
  async fetchRoomPins(spaceId: string, channelId: string): Promise<import("./wasm-types").WasmPinEntry[]> {
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    if (typeof xmpp.fetch_room_pins !== "function") throw new Error("fetch_room_pins not available in wasm client");
    const result = await xmpp.fetch_room_pins(roomJid);
    return Array.isArray(result) ? (result as import("./wasm-types").WasmPinEntry[]) : [];
  }
  /** XEP-0359: fetch a batch of MAM messages by their stanza-ids. */
  async fetchRoomMessagesByStanzaIds(
    spaceId: string,
    channelId: string,
    stanzaIds: string[],
  ): Promise<import("./wasm-types").WasmArchivedMessage[]> {
    if (stanzaIds.length === 0) return [];
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    if (typeof xmpp.fetch_room_messages_by_stanza_ids !== "function") throw new Error("fetch_room_messages_by_stanza_ids not available in wasm client");
    const page = (await xmpp.fetch_room_messages_by_stanza_ids(roomJid, stanzaIds)) as import("./wasm-types").WasmMamPage | null;
    return Array.isArray(page?.messages) ? page.messages : [];
  }
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
  async discoverExtensionRoutes(): Promise<DiscoveredExtensionRoute[]> { const xmpp = await this.requireConnectedXmpp(); return discoverExtensionRoutes(xmpp as WasmClient, this.session.jid); }
  async fetchExtensionRouteItems(route: DiscoveredExtensionRoute, roomJid: string): Promise<ExtensionRouteItem[]> { const xmpp = await this.requireConnectedXmpp(); return fetchExtensionRouteItems(xmpp as WasmClient, route, roomJid); }
  async invokeExtensionCommand(command: DiscoveredExtensionCommand): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return invokeExtensionCommand(xmpp as WasmClient, this.session.jid, command); }
  async submitExtensionCommandForm(command: DiscoveredExtensionCommand, sessionId: string, fields: ExtensionCommandFormField[], action?: ExtensionCommandAction, roomJid?: string): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return submitExtensionCommandForm(xmpp as WasmClient, command, sessionId, fields, action, roomJid); }

  async enablePushNotifications(opts: { serviceJid: string; node?: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.enable_push_notifications) return false;
    try { await xmpp.enable_push_notifications(opts.serviceJid, opts.node ?? "web-push", ""); return true; } catch { return false; }
  }

  async disablePushNotifications(opts: { serviceJid: string; node?: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.disable_push_notifications) return false;
    try { await xmpp.disable_push_notifications(opts.serviceJid, opts.node ?? "web-push"); return true; } catch { return false; }
  }

  async fetchInbox(opts: FetchInboxOptions = {}): Promise<InboxResult> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.fetch_inbox?.({ ...(typeof opts.since === "number" ? { since: opts.since } : {}), ...(opts.onlyUnread ? { only_unread: true } : {}), ...(opts.room ? { room: opts.room } : {}), ...(opts.threads ? { threads: true } : {}) }) as WasmInboxResult | undefined;
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
  private recordRoomMamWatermarks(messages: ReadonlyArray<LiveRoomMessage>) {
    for (const message of messages) {
      this.catchup.recordRoomSeen(message.roomJid, message.createdAt, message.archiveId, messageSeenIds(message));
    }
  }
  private recordDmMamWatermarks(messages: ReadonlyArray<LiveDmMessage>) {
    for (const message of messages) {
      this.catchup.recordDmSeen(message.peerJid, message.createdAt, message.archiveId, messageSeenIds(message));
    }
  }

  async queryMam(spaceId: string, channelId: string, max = 50): Promise<LiveRoomMessage[]> { const page = await this.queryMamPage(spaceId, channelId, max, { type: "latest" }); return page.messages; }
  async queryMamPage(spaceId: string, channelId: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_page?.(this.roomJidForChannel(channelId), max, pageParam) as WasmMamPage;
    if (!page) return { messages: [], complete: true };
    const result = this.roomMamPageToMessages(page);
    this.recordRoomMamWatermarks(result.messages);
    return result;
  }
  async queryMamByThread(spaceId: string, channelId: string, threadId: string, max = 100): Promise<LiveRoomMessage[]> {
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_by_thread?.(this.roomJidForChannel(channelId), threadId, max, null) as WasmMamPage;
    if (!page) return [];
    const result = this.roomMamPageToMessages(page);
    this.recordRoomMamWatermarks(result.messages);
    return result.messages;
  }
  async queryMamThreadPage(spaceId: string, channelId: string, threadId: string, max = 100, pageParam: MamThreadPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    if (!threadId) return { messages: [], complete: true };
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_by_thread?.(this.roomJidForChannel(channelId), threadId, max, pageParam.type === "before" ? pageParam.before : null) as WasmMamPage;
    if (!page) return { messages: [], complete: true };
    const result = this.roomMamPageToMessages(page);
    this.recordRoomMamWatermarks(result.messages);
    return result;
  }
  async searchMessages(_spaceId: string, channelId: string, query: string, max = 20): Promise<MessageSearchResult[]> {
    if (!query.trim()) return [];
    const xmpp = await this.requireConnectedXmpp();
    const page = await xmpp.search_room_history?.(this.roomJidForChannel(channelId), query, max) as WasmMamPage;
    const parsed = page ? this.roomMamPageToMessages(page).messages : [];
    return parsed.filter((message) => !!message.body).map((message, index) => ({ id: message.id, ...(page?.messages[index]?.mam_id ? { archiveId: page.messages[index].mam_id } : {}), nick: message.nick, body: message.body, createdAt: message.createdAt, ...(message.threadId ? { threadId: message.threadId } : {}), ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}), roomJid: message.roomJid }));
  }
  async queryPersonalMam(peerJid: string, max = 100): Promise<LiveDmMessage[]> { const page = await this.queryPersonalMamPage(peerJid, max, { type: "latest" }); return page.messages; }
  async queryPersonalMamPage(peerJid: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveDmMessage>> {
    const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_dm_history_page?.(barePeerJid(peerJid), max, pageParam) as WasmMamPage;
    if (!page) return { messages: [], complete: true };
    const result = this.dmMamPageToMessages(page);
    this.recordDmMamWatermarks(result.messages);
    return result;
  }
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
      : null;
    if (!listMembers) { this.emitError({ kind: "member-query", recoverable: false, detail: "missing list_room_members" }); throw new Error("missing list_room_members"); }
    const affiliations = ["owner", "admin", "member", "outcast"] as const; const members: MemberSummary[] = []; const failedAffiliations: string[] = [];
    for (const affiliation of affiliations) {
      try { const result = await listMembers(affiliation); for (const item of result ?? []) { if (!item.jid) continue; members.push({ jid: item.jid, username: item.jid.split("@")[0] ?? item.jid, avatar_url: null, role: affiliation, joined_at: "" }); } } catch (error: any) { failedAffiliations.push(affiliation); const condition = error?.condition ?? error?.error?.condition; const detail = condition === "forbidden" ? `forbidden affiliation query — ${roomJid}` : condition === "service-unavailable" ? `unsupported member query — ${roomJid}` : `affiliation query failed for ${affiliation} — ${roomJid}; reconstructed room JID may not match`; this.emitError({ kind: "member-query", recoverable: true, detail, cause: error, condition }); }
    }
    if (members.length === 0 && failedAffiliations.length > 0) {
      throw new RoomMemberListUnavailableError();
    }
    return members;
  }
  async setRoomAffiliation(channelId: string, jid: string, affiliation: MemberSummary["role"]): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.set_room_affiliation?.(this.roomJidForChannel(channelId), jid, affiliation === "none" ? "none" : affiliation); }
  async searchUsers(query: string): Promise<UserSearchResult[]> { if (!query.trim()) return []; const xmpp = await this.requireConnectedXmpp(); const users = await xmpp.search_users?.(query) as WasmUserSearchResult[]; return (users ?? []).map((user) => ({ id: user.jid, jid: user.jid, username: user.username ?? user.nick ?? user.jid.split("@")[0] ?? user.jid, display_name: user.display_name ?? user.name ?? null, avatar_url: null })); }
  async fetchUserAvatar(jid: string): Promise<string | null> {
    const xmpp = await this.requireConnectedXmpp();
    const bareJid = barePeerJid(jid);
    if (xmpp.request_avatar) {
      const avatar = await xmpp.request_avatar(bareJid) as WasmAvatar | null;
      if (avatar?.url) return avatar.url;
      if (avatar?.data) return avatarDataUrl(avatar.data, avatar.mime_type);
    }
    return null;
  }
  get agent(): XmppClientInstance | null { return this.xmpp; }

  private startSelfPing() { this.stopSelfPing(); this.selfPingTimer = setInterval(() => { void this.doSelfPing(); }, 60000); }
  private stopSelfPing() { if (this.selfPingTimer) { clearInterval(this.selfPingTimer); this.selfPingTimer = null; } }
  private async doSelfPing() { if (!this.xmpp?.send_raw_iq || !this.currentRoom) return; try { await this.xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${this.currentRoom}/${this.session.username}"><ping xmlns="urn:ietf:params:xml:ns:xmpp-ping"/></iq>`); } catch { this.roomDisconnectHandler?.(); } }
  private handleMessageAck(id: string) { const wasQueued = this.inflightQueuedIds.delete(id); if (wasQueued) removeQueuedMessage(this.queueScope, id); this.messageAckHandler?.(id); const pending = this.pendingSendAt.get(id); if (pending) { this.pendingSendAt.delete(id); this.fireHook(this.messageAckHooks, id, { kind: pending.kind, latencyMs: performance.now() - pending.at }); } if (wasQueued) this.emitQueueDepth(); }
  private handleMessageFailed(id: string) { const wasQueued = this.inflightQueuedIds.delete(id); this.messageDeliveryFailureHandler?.(id); const pending = this.pendingSendAt.get(id); if (pending) { this.pendingSendAt.delete(id); this.fireHook(this.messageFailHooks, id, { kind: pending.kind }); } if (wasQueued) this.emitQueueDepth(); }
  private handleSessionReady(xmpp: XmppClientInstance, lifecycle: SessionLifecycleEvent) {
    if (this.xmpp !== xmpp) return;
    this.connected = true; this.reconnectAttempt = 0;
    this.emitStatus({ state: "online", detail: countQueuedMessages(this.queueScope) > 0 ? lifecycle.type === "fresh" ? "Reconnected — replaying queued messages" : "Connection resumed — replaying queued messages" : lifecycle.type === "fresh" ? "Connection ready" : "Connection resumed" });
    const catchupEntries = this.catchup.onSessionStarted();
    if (lifecycle.type === "fresh") { this.inflightQueuedIds.clear(); void this.enableCarbons(xmpp); }
    if (catchupEntries.length > 0) { void this.runReconnectCatchup(xmpp, catchupEntries); }
    this.emitSessionLifecycle(lifecycle); void this.flushQueuedDirectMessages(); if (this.currentRoom) void this.flushQueuedRoomMessages(this.currentRoom);
    if (this.onceConnected) { const done = this.onceConnected; this.onceConnected = null; this.onceConnectFailed = null; done(); }
  }
  private handleDisconnected(xmpp: XmppClientInstance, error?: Error) {
    if (this.xmpp !== xmpp) return;
    this.connected = false; this.stopSelfPing(); this.xmpp = null;
    if (this.destroying) { this.clearResumeState(); this.emitStatus({ state: "offline", detail: error?.message ?? "Disconnected" }); return; }
    this.setResumeStateHandle(xmpp.get_resume_state_handle?.() ?? null);
    this.resumeState = xmpp.get_resume_state?.() ?? null;
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
    if (message.reaction_target_id) { if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; this.reactionHandler?.({ roomJid, nick, messageId: message.reaction_target_id, emojis: message.reaction_emojis }); } else { const fromBare = barePeerJid(message.from ?? ""); const toBare = barePeerJid(message.to ?? ""); const selfBare = barePeerJid(this.session.jid); const peerJid = fromBare === selfBare ? toBare : fromBare; const reactorJid = fromBare || selfBare; if (peerJid && reactorJid) this.dmReactionHandler?.({ peerJid, reactorJid, messageId: message.reaction_target_id, emojis: message.reaction_emojis }); } return; }
    if (message.pin_event && message.is_muc) {
      const roomJid = barePeerJid(message.from ?? message.to ?? "");
      this.pinEventHandler?.({ roomJid, event: message.pin_event });
      // Fall through: also render the system message in the timeline so
      // the user sees "alice pinned a message" inline (#414). The
      // pin store update has already happened above.
    }
    if (message.is_muc) {
      const converted = roomMessageFromArchived({ ...message, mam_id: message.id ?? crypto.randomUUID() } as WasmArchivedMessage);
      if (!converted) return;
      this.catchup.recordRoomSeen(converted.roomJid, converted.createdAt, undefined, rawMessageSeenIds(message));
      if (converted.roomJid !== this.currentRoom && isRoomActivityMessage(converted)) { this.activityHandler?.(roomActivityEventFromMessage(converted)); return; }
      this.messageHandler?.(converted); return;
    }
    const converted = dmMessageFromArchived({ ...message, mam_id: message.id ?? crypto.randomUUID() } as WasmArchivedMessage, barePeerJid(this.session.jid));
    if (converted) {
      this.catchup.recordDmSeen(converted.peerJid, converted.createdAt, undefined, rawMessageSeenIds(message));
      this.directMessageHandler?.(converted);
    }
  }
  private async runReconnectCatchup(
    xmpp: XmppClientInstance,
    entries: Array<{ kind: "dm" | "room"; key: string; after?: string; since?: string; seenIds?: string[] }>,
  ) {
    for (const entry of entries) {
      if (this.xmpp !== xmpp) return;
      try {
        if (entry.kind === "dm") {
          await this.runDmReconnectCatchup(xmpp, entry);
        } else {
          await this.runRoomReconnectCatchup(xmpp, entry);
        }
      } catch (error) {
        this.emitError({
          kind: "history",
          recoverable: true,
          detail: `Reconnect catch-up failed for ${entry.key}`,
          cause: error,
        });
      }
    }
  }
  private async runDmReconnectCatchup(
    xmpp: XmppClientInstance,
    entry: { key: string; after?: string; since?: string; seenIds?: string[] },
  ) {
    if (!xmpp.fetch_dm_history_page) return;
    if (entry.after) {
      let after: string | undefined = entry.after;
      const seenAfter = new Set<string>();
      while (after) {
        if (seenAfter.has(after)) throw new Error(`Reconnect catch-up repeated archive cursor for ${entry.key}`);
        seenAfter.add(after);
        const page = await xmpp.fetch_dm_history_page(entry.key, 100, { type: "after", after }) as WasmMamPage;
        const nextAfter = this.applyDmCatchupPage(page, undefined, entry.seenIds);
        if (isMamPageComplete(page)) return;
        if (!nextAfter) throw new Error(`Reconnect catch-up could not advance archive cursor for ${entry.key}`);
        after = nextAfter;
      }
      return;
    }
    const since = entry.since ?? this.catchup.getDmLastSeen(entry.key);
    if (!since) return;
    await this.runDmTimestampCatchup(xmpp, entry.key, since, entry.seenIds);
  }
  private async runRoomReconnectCatchup(
    xmpp: XmppClientInstance,
    entry: { key: string; after?: string; since?: string; seenIds?: string[] },
  ) {
    if (!xmpp.fetch_room_history_page) return;
    if (entry.after) {
      let after: string | undefined = entry.after;
      const seenAfter = new Set<string>();
      while (after) {
        if (seenAfter.has(after)) throw new Error(`Reconnect catch-up repeated archive cursor for ${entry.key}`);
        seenAfter.add(after);
        const page = await xmpp.fetch_room_history_page(entry.key, 100, { type: "after", after }) as WasmMamPage;
        const nextAfter = this.applyRoomCatchupPage(page, undefined, entry.seenIds);
        if (isMamPageComplete(page)) return;
        if (!nextAfter) throw new Error(`Reconnect catch-up could not advance archive cursor for ${entry.key}`);
        after = nextAfter;
      }
      return;
    }
    const since = entry.since ?? this.catchup.getRoomLastSeen(entry.key);
    if (!since) return;
    await this.runRoomTimestampCatchup(xmpp, entry.key, since, entry.seenIds);
  }
  private async runDmTimestampCatchup(
    xmpp: XmppClientInstance,
    peerJid: string,
    since: string,
    seenIds?: ReadonlyArray<string>,
  ) {
    let pageParam: MamPageParam = { type: "latest" };
    const seenBefore = new Set<string>();
    while (true) {
      const page = await xmpp.fetch_dm_history_page?.(peerJid, 100, pageParam) as WasmMamPage;
      this.applyDmCatchupPage(page, since, seenIds);
      if (isMamPageComplete(page) || pageCrossesSince(page, since)) return;
      const firstArchiveId = pageFirstArchiveId(page);
      if (!firstArchiveId) throw new Error(`Reconnect catch-up could not page backward for ${peerJid}`);
      if (seenBefore.has(firstArchiveId)) throw new Error(`Reconnect catch-up repeated backward archive cursor for ${peerJid}`);
      seenBefore.add(firstArchiveId);
      pageParam = { type: "before", before: firstArchiveId };
    }
  }
  private async runRoomTimestampCatchup(
    xmpp: XmppClientInstance,
    roomJid: string,
    since: string,
    seenIds?: ReadonlyArray<string>,
  ) {
    let pageParam: MamPageParam = { type: "latest" };
    const seenBefore = new Set<string>();
    while (true) {
      const page = await xmpp.fetch_room_history_page?.(roomJid, 100, pageParam) as WasmMamPage;
      this.applyRoomCatchupPage(page, since, seenIds);
      if (isMamPageComplete(page) || pageCrossesSince(page, since)) return;
      const firstArchiveId = pageFirstArchiveId(page);
      if (!firstArchiveId) throw new Error(`Reconnect catch-up could not page backward for ${roomJid}`);
      if (seenBefore.has(firstArchiveId)) throw new Error(`Reconnect catch-up repeated backward archive cursor for ${roomJid}`);
      seenBefore.add(firstArchiveId);
      pageParam = { type: "before", before: firstArchiveId };
    }
  }
  private applyDmCatchupPage(page: WasmMamPage | null | undefined, since?: string, seenIds?: ReadonlyArray<string>): string | undefined {
    let lastArchiveId = pageLastArchiveId(page);
    for (const message of page?.messages ?? []) {
      const converted = dmMessageFromArchived(message, barePeerJid(this.session.jid));
      if (!converted || shouldSkipCatchupMessage(converted, since, seenIds)) continue;
      this.catchup.recordDmSeen(converted.peerJid, converted.createdAt, converted.archiveId, messageSeenIds(converted));
      this.directMessageHandler?.(converted);
      lastArchiveId = converted.archiveId ?? lastArchiveId;
    }
    return lastArchiveId;
  }
  private applyRoomCatchupPage(page: WasmMamPage | null | undefined, since?: string, seenIds?: ReadonlyArray<string>): string | undefined {
    let lastArchiveId = pageLastArchiveId(page);
    for (const message of page?.messages ?? []) {
      const converted = roomMessageFromArchived(message);
      if (!converted || shouldSkipCatchupMessage(converted, since, seenIds)) continue;
      this.catchup.recordRoomSeen(converted.roomJid, converted.createdAt, converted.archiveId, messageSeenIds(converted));
      if (converted.roomJid !== this.currentRoom && isRoomActivityMessage(converted)) {
        this.activityHandler?.(roomActivityEventFromMessage(converted));
      } else {
        this.messageHandler?.(converted);
      }
      lastArchiveId = converted.archiveId ?? lastArchiveId;
    }
    return lastArchiveId;
  }
  private wireEvents(xmpp: XmppClientInstance & { enableKeepAlive?: (opts: { interval: number; timeout: number }) => void; disableKeepAlive?: () => void }) {
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

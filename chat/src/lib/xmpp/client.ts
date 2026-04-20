/** BrowserXmppClient — thin orchestrator that delegates to functional modules. */
import { createClient } from "stanza";
import type { Agent } from "stanza";
import type { ReceivedMUCPresence, ReceivedMessage, ReceivedPresence } from "stanza/protocol";
import type { WaddleSession } from "../server-auth";
import type { WaddleHat } from "./extensions/hats";
import type {
  ChatStateEvent, ChatStateType, DiscoveredChannel, DiscoveredWaddle, DisplayedEvent,
  DmChatStateEvent, DmDisplayedEvent, DmReactionEvent, LiveDmMessage, LiveRoomMessage,
  OccupantPresence, PresenceUpdateEvent, ReactionEvent, RoomActivityEvent,
  RoomHats, RoomPresence, SessionLifecycleEvent, XmppStatusSnapshot,
} from "./types";
import { barePeerJid, jidDomain, roomBareJidFor } from "./jid";
import { registerWaddleExtensions } from "./extensions";
import { ext } from "./message-parsing";
import { dispatchChat } from "./dm-parsing";
import { buildMessageDispatcher } from "./message-dispatch";
import * as messaging from "./messaging";
import * as history from "./history";
import * as dmMessaging from "./dm-messaging";
import * as dmHistory from "./dm-history";
import * as discovery from "./discovery";
import { prepareEncryptedAttachmentUpload } from "./encrypted-attachments";
import { discoverUploadService, uploadFile, type UploadProgress } from "./file-upload";
import {
  countQueuedMessages,
  enqueueQueuedMessage,
  listQueuedMessages,
  listQueuedRoomMessages,
  removeQueuedMessage,
  type PersistedQueuedDmMessage,
} from "../outbound-queue-store";
import * as inboxApi from "./inbox";
import * as pep from "./pep-publications";
import { ReconnectCatchup } from "./reconnect-catchup";

// Cap MAM catch-up per conversation so a long offline period can't drown the
// client. In practice, web clients reconnect within minutes; 200 is generous.
const CATCHUP_MAX_PER_CONVERSATION = 200;

type StanzaSaslMechanism = { name: string };
type StanzaSaslFactory = {
  disable(mechanism: string): void;
  mechanisms?: StanzaSaslMechanism[];
};

const DISABLED_SASL_MECHANISMS = [
  "EXTERNAL",
  "SCRAM-SHA-256-PLUS",
  "SCRAM-SHA-256",
  "SCRAM-SHA-1-PLUS",
  "SCRAM-SHA-1",
  "DIGEST-MD5",
  "X-OAUTH2",
  "PLAIN",
  "ANONYMOUS",
];

function createXmppResource() {
  const randomId = globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
  return `web-${randomId}`;
}

function keepOnlyOAuthBearer(xmpp: Agent) {
  const sasl = xmpp.sasl as unknown as StanzaSaslFactory;
  for (const mech of DISABLED_SASL_MECHANISMS) {
    sasl.disable(mech);
  }

  if (Array.isArray(sasl.mechanisms)) {
    const oauthBearer = sasl.mechanisms.filter(
      (mechanism) => mechanism.name.toUpperCase() === "OAUTHBEARER",
    );
    if (oauthBearer.length === 0) {
      throw new Error("Stanza OAUTHBEARER SASL mechanism is unavailable");
    }
    sasl.mechanisms = oauthBearer;
  }
}

function parsePresenceShow(show: string | undefined): OccupantPresence {
  switch (show) {
    case "away":
    case "xa":    return "away";
    case "dnd":   return "dnd";
    case "chat":  return "online";
    default:      return "online";
  }
}

function parsePresenceHats(value: unknown): WaddleHat[] {
  const hats = Array.isArray(value) ? value : value ? [value] : [];
  return hats
    .map((hat) => hat as Partial<WaddleHat>)
    .map((hat) => ({ title: hat.title ?? "", uri: hat.uri ?? "" }))
    .filter((hat) => hat.title && hat.uri);
}

const OPTIONAL_XMPP_FEATURE_ERROR_CONDITIONS = new Set([
  "feature-not-implemented",
  "service-unavailable",
  "item-not-found",
  "remote-server-timeout",
]);

function xmppErrorCondition(error: unknown): string | null {
  if (!error || typeof error !== "object") return null;
  const candidate = error as Record<string, unknown>;
  const direct = candidate.condition;
  if (typeof direct === "string" && direct.length > 0) return direct;

  const nested = candidate.error;
  if (nested && typeof nested === "object") {
    const nestedCondition = (nested as Record<string, unknown>).condition;
    if (typeof nestedCondition === "string" && nestedCondition.length > 0) return nestedCondition;
  }

  return null;
}

function isOptionalXmppFeatureError(error: unknown): boolean {
  const condition = xmppErrorCondition(error);
  return !!condition && OPTIONAL_XMPP_FEATURE_ERROR_CONDITIONS.has(condition);
}

function isLocalAvailabilityError(error: unknown): boolean {
  return error instanceof Error
    && (
      error.message === "XMPP session is not ready"
      || error.message.startsWith("Room is not ready:")
      || error.message === "Reconnection timed out"
    );
}

function mapPresenceShow(pres: ReceivedPresence): PresenceUpdateEvent["show"] {
  if ((pres.type ?? "available") === "unavailable") return "offline";
  switch (pres.show ?? "available") {
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

interface OutboundSendResult {
  id: string | null;
  state: "queued" | "sending";
}

export class BrowserXmppClient {
  private session: WaddleSession;
  private get queueScope() { return barePeerJid(this.session.jid); }
  private readonly resource = createXmppResource();
  private messageHandler: ((message: LiveRoomMessage) => void) | null = null;
  private directMessageHandler: ((message: LiveDmMessage) => void) | null = null;
  private statusHandler: ((status: XmppStatusSnapshot) => void) | null = null;
  private reactionHandler: ((event: ReactionEvent) => void) | null = null;
  private displayedHandler: ((event: DisplayedEvent) => void) | null = null;
  private chatStateHandler: ((event: ChatStateEvent) => void) | null = null;
  private dmChatStateHandler: ((event: DmChatStateEvent) => void) | null = null;
  private dmReactionHandler: ((event: DmReactionEvent) => void) | null = null;
  private dmDisplayedHandler: ((event: DmDisplayedEvent) => void) | null = null;
  private presenceUpdateHandler: ((event: PresenceUpdateEvent) => void) | null = null;
  private memberJidHandler: ((nick: string, bareJid: string) => void) | null = null;
  private hatsHandler: ((hats: RoomHats) => void) | null = null;
  private slowModeHandler: ((seconds: number) => void) | null = null;
  private activityHandler: ((event: RoomActivityEvent) => void) | null = null;
  private inboxPushHandler: ((entry: inboxApi.InboxEntry) => void) | null = null;
  private roomAvatarHandler: ((roomJid: string, hash: string) => void) | null = null;
  private roomDisconnectHandler: (() => void) | null = null;
  private presenceHandler: ((presence: RoomPresence) => void) | null = null;
  private lastSeenHandler: ((nick: string, timestamp: number) => void) | null = null;
  private messageAckHandler: ((messageId: string) => void) | null = null;
  private messageDeliveryFailureHandler: ((messageId: string) => void) | null = null;
  private queuedMessageStatusHandler:
    ((messageId: string, status: "queued" | "sending") => void) | null = null;
  private sessionLifecycleHandler: ((event: SessionLifecycleEvent) => void) | null = null;
  private xmpp: Agent | null = null;
  private connectPromise: Promise<void> | null = null;
  private connected = false;
  private destroying = false;
  // Slice A: MAM-on-reconnect tracker. Updated on every live room / DM /
  // carbon message, consulted on every `session:started` after the first.
  private readonly catchup = new ReconnectCatchup();
  private refreshSession: (() => Promise<WaddleSession | null>) | null = null;
  private currentRoom: string | null = null;
  private roomSwitchPromise: Promise<void> | null = null;
  private roomSwitchTarget: string | null = null;
  private selfPingTimer: ReturnType<typeof setInterval> | null = null;
  private roomHats: RoomHats = {};
  private roomPresence: RoomPresence = {};
  private uploadServiceJid: string | null = null;
  private visibilityListener: (() => void) | null = null;
  private onlineListener: (() => void) | null = null;
  private reconnectNudgeAt = 0;
  private lastStanzaKickAt = 0;
  private directQueueFlushPromise: Promise<void> | null = null;
  private readonly roomQueueFlushes = new Map<string, Promise<void>>();

  constructor(session: WaddleSession) { this.session = session; }

  setMessageHandler(h: (message: LiveRoomMessage) => void) { this.messageHandler = h; }
  setDirectMessageHandler(h: (message: LiveDmMessage) => void) { this.directMessageHandler = h; }
  setStatusHandler(h: (status: XmppStatusSnapshot) => void) { this.statusHandler = h; }
  setChatStateHandler(h: (event: ChatStateEvent) => void) { this.chatStateHandler = h; }
  setDmChatStateHandler(h: (event: DmChatStateEvent) => void) { this.dmChatStateHandler = h; }
  setReactionHandler(h: (event: ReactionEvent) => void) { this.reactionHandler = h; }
  setDmReactionHandler(h: (event: DmReactionEvent) => void) { this.dmReactionHandler = h; }
  setDisplayedHandler(h: (event: DisplayedEvent) => void) { this.displayedHandler = h; }
  setDmDisplayedHandler(h: (event: DmDisplayedEvent) => void) { this.dmDisplayedHandler = h; }
  setPresenceUpdateHandler(h: (event: PresenceUpdateEvent) => void) { this.presenceUpdateHandler = h; }
  setMemberJidHandler(h: (nick: string, bareJid: string) => void) { this.memberJidHandler = h; }
  setHatsHandler(h: (hats: RoomHats) => void) { this.hatsHandler = h; }
  setSlowModeHandler(h: (seconds: number) => void) { this.slowModeHandler = h; }
  setActivityHandler(h: (event: RoomActivityEvent) => void) { this.activityHandler = h; }
  setInboxPushHandler(h: (entry: inboxApi.InboxEntry) => void) { this.inboxPushHandler = h; }
  setRoomAvatarHandler(h: (roomJid: string, hash: string) => void) { this.roomAvatarHandler = h; }
  setRoomDisconnectHandler(h: () => void) { this.roomDisconnectHandler = h; }
  setPresenceHandler(h: (presence: RoomPresence) => void) { this.presenceHandler = h; }
  setLastSeenHandler(h: (nick: string, timestamp: number) => void) { this.lastSeenHandler = h; }
  setMessageAckHandler(h: (messageId: string) => void) { this.messageAckHandler = h; }
  setMessageDeliveryFailureHandler(h: (messageId: string) => void) { this.messageDeliveryFailureHandler = h; }
  setQueuedMessageStatusHandler(h: (messageId: string, status: "queued" | "sending") => void) {
    this.queuedMessageStatusHandler = h;
  }
  setSessionLifecycleHandler(h: (event: SessionLifecycleEvent) => void) { this.sessionLifecycleHandler = h; }
  setRefreshSession(fn: () => Promise<WaddleSession | null>) { this.refreshSession = fn; }

  private updateSession(session: WaddleSession, xmpp: Agent | null = this.xmpp) {
    this.session = session;
    if (!xmpp) return;
    (xmpp.config as any).jid = session.jid;
    (xmpp.config as any).credentials = { token: session.session_id };
    (xmpp.config as any).transports = { websocket: session.xmpp_websocket_url, bosh: false };
  }

  private discardDisconnectedAgent(xmpp: Agent) {
    if (this.xmpp === xmpp) {
      this.connected = false;
      this.connectPromise = null;
      this.stopSelfPing();
      this.stopKeepAlive();
      this.roomSwitchPromise = null;
      this.roomSwitchTarget = null;
      this.uploadServiceJid = null;
      this.roomHats = {};
      this.hatsHandler?.({});
      this.roomPresence = {};
      this.presenceHandler?.({});
      this.xmpp = null;
    }
    try { xmpp.disconnect(); } catch { /* ignore stale client */ }
  }

  // -- Connection lifecycle --

  async connect(): Promise<void> {
    if (this.xmpp && this.connected) return;
    if (this.connectPromise) return this.connectPromise;

    // If the Agent exists but is disconnected, actively kick stanza's
    // reconnect rather than relying solely on its internal backoff — in
    // practice autoReconnect can stall indefinitely after a network blip.
    // Wait for either a fresh bind (session:started) or an XEP-0198 resume
    // (stream:management:resumed); resume does NOT emit session:started
    // because feature negotiation is short-circuited.
    if (this.xmpp && !this.destroying) {
      const xmpp = this.xmpp;
      const reconnectPromise = new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => {
          cleanup();
          this.connectPromise = null;
          reject(new Error("Reconnection timed out"));
        }, 15_000);
        const cleanup = () => {
          clearTimeout(timeout);
          xmpp.off("session:started", onReady);
          xmpp.off("stream:management:resumed", onReady);
          xmpp.off("disconnected", onDisconnected);
        };
        const onReady = () => { cleanup(); this.connectPromise = null; resolve(); };
        // Intentional teardown (logout / unmount) sets this.destroying and
        // calls xmpp.disconnect(); stanza then fires `disconnected`.  Abort
        // any in-flight awaiters instead of making them wait for the 15s
        // timeout.  Mid-reconnect drops keep waiting — stanza will re-fire
        // session:started or stream:management:resumed.
        const onDisconnected = () => {
          if (!this.destroying) return;
          cleanup();
          this.connectPromise = null;
          reject(new Error("Client disconnected"));
        };
        xmpp.on("session:started", onReady);
        xmpp.on("stream:management:resumed", onReady);
        xmpp.on("disconnected", onDisconnected);
      });
      this.connectPromise = reconnectPromise.catch(async (error): Promise<void> => {
        if (
          error instanceof Error
          && error.message === "Reconnection timed out"
          && !this.destroying
          && this.xmpp === xmpp
        ) {
          this.discardDisconnectedAgent(xmpp);
          return this.connect();
        }
        throw error;
      });
      if (Date.now() - this.lastStanzaKickAt > 2_000) {
        this.lastStanzaKickAt = Date.now();
        try { xmpp.connect(); } catch { /* stanza may be mid-handshake */ }
      }
      return this.connectPromise;
    }

    // Fresh connection — tear down any stale agent
    if (this.xmpp) {
      try { this.xmpp.disconnect(); } catch { /* ignore stale client */ }
      this.xmpp = null;
    }
    this.destroying = false;
    this.roomSwitchPromise = null;
    this.roomSwitchTarget = null;
    this.roomHats = {};
    this.hatsHandler?.({});
    this.roomPresence = {};
    this.presenceHandler?.({});
    const xmpp = createClient({
      jid: this.session.jid,
      // The session_id is a bearer token — use OAUTHBEARER (RFC 7628)
      credentials: { token: this.session.session_id },
      resource: this.resource,
      transports: { websocket: this.session.xmpp_websocket_url, bosh: false },
      autoReconnect: true,
      // XEP-0198: per-stanza acks + session resume. stanza.js tracks unacked
      // stanzas in memory and the server replays them on successful resume,
      // closing the window where transient 503s or transport flaps lose
      // messages.
      useStreamManagement: true,
      sendReceipts: false, chatMarkers: false,
    });
    // Only keep OAUTHBEARER; session tokens aren't SCRAM/PLAIN passwords
    keepOnlyOAuthBearer(xmpp);
    registerWaddleExtensions(xmpp);
    this.wireEvents(xmpp);
    this.xmpp = xmpp;
    this.startReconnectNudgeListeners();

    // Wait for session:started (fresh bind) or stream:management:resumed (SM
    // resume). Initial connects always take the fresh path, but subsequent
    // auto-reconnects may resume — only one of the two will fire.
    const promise = new Promise<void>((resolve, reject) => {
      const cleanup = () => {
        xmpp.off("session:started", onReady);
        xmpp.off("stream:management:resumed", onReady);
        xmpp.off("disconnected", onFail);
      };
      const onReady = () => { cleanup(); this.connected = true; this.connectPromise = null; resolve(); };
      const onFail = (err?: Error) => {
        cleanup();
        if (this.xmpp === xmpp) {
          this.connected = false;
          this.xmpp = null;
        }
        this.connectPromise = null;
        reject(err ?? new Error("XMPP connection failed"));
      };
      xmpp.on("session:started", onReady);
      xmpp.on("stream:management:resumed", onReady);
      xmpp.on("disconnected", onFail);
    });
    this.connectPromise = promise;
    xmpp.connect();
    return promise;
  }

  async disconnect() {
    if (!this.xmpp) return;
    this.destroying = true;
    const currentRoom = this.currentRoom;
    this.roomSwitchPromise = null;
    this.roomSwitchTarget = null;
    if (currentRoom) {
      try { await this.xmpp.leaveRoom(currentRoom, this.session.username); } catch { /* best-effort */ }
    }
    this.stopSelfPing();
    this.stopKeepAlive();
    this.stopReconnectNudgeListeners();
    const xmpp = this.xmpp;
    this.xmpp = null;
    this.connectPromise = null;
    this.connected = false;
    this.currentRoom = null;
    this.uploadServiceJid = null;
    // Intentional teardown — next connect is a fresh session, not a resume.
    this.catchup.reset();
    try { xmpp.disconnect(); } catch { /* ignore */ }
  }

  // -- Room management --

  async switchRoom(waddleId: string, channelId: string) {
    await this.connect();
    const nextRoom = roomBareJidFor(this.session, waddleId, channelId);

    const pendingSwitch = this.roomSwitchPromise;
    if (pendingSwitch) {
      if (this.roomSwitchTarget === nextRoom) {
        await pendingSwitch;
        return;
      }
      await pendingSwitch.catch(() => undefined);
    }

    if (this.currentRoom === nextRoom) return;

    const switchPromise = this.performRoomSwitch(nextRoom);
    this.roomSwitchPromise = switchPromise;
    this.roomSwitchTarget = nextRoom;
    try {
      await switchPromise;
    } finally {
      if (this.roomSwitchPromise === switchPromise) {
        this.roomSwitchPromise = null;
        this.roomSwitchTarget = null;
      }
    }
  }

  private async performRoomSwitch(nextRoom: string) {
    if (this.currentRoom && this.xmpp) {
      try { await this.xmpp.leaveRoom(this.currentRoom, this.session.username); } catch { /* best-effort */ }
    }
    this.currentRoom = nextRoom;
    this.roomHats = {};
    this.hatsHandler?.({});
    this.roomPresence = {};
    this.presenceHandler?.({});
    const xmpp = this.xmpp;
    if (!xmpp) return;

    try {
      const fallbackJoin = this.waitForRoomSelfPresence(xmpp, nextRoom, this.session.username);
      const stanzaJoin = xmpp.joinRoom(nextRoom, this.session.username);
      void stanzaJoin.catch(() => undefined);

      await Promise.race([stanzaJoin.then(() => undefined), fallbackJoin]);
      xmpp.joinedRooms?.set(nextRoom, this.session.username);
      xmpp.joiningRooms?.delete(nextRoom);
      if (this.xmpp !== xmpp || this.currentRoom !== nextRoom) return;
      this.startSelfPing();
      void this.flushQueuedRoomMessages(nextRoom);
    } catch (error) {
      if (this.currentRoom === nextRoom) {
        this.currentRoom = null;
        this.stopSelfPing();
        this.roomHats = {};
        this.hatsHandler?.({});
        this.roomPresence = {};
        this.presenceHandler?.({});
      }
      throw error;
    }
  }

  private waitForRoomSelfPresence(
    xmpp: Agent,
    roomJid: string,
    nick: string,
  ): Promise<void> {
    const fullJid = `${roomJid}/${nick}`;

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        cleanup();
        reject(new Error(`Timed out waiting for self-presence in ${roomJid}`));
      }, 10_000);

      const cleanup = () => {
        clearTimeout(timeout);
        xmpp.off("muc:available", onMucPresence);
        xmpp.off("disconnected", onDisconnected);
      };

      const onMucPresence = (pres: ReceivedMUCPresence) => {
        const from = pres.from ?? "";
        if (from === fullJid) {
          cleanup();
          resolve();
        }
      };

      const onDisconnected = (error?: Error) => {
        cleanup();
        reject(error ?? new Error("XMPP disconnected while joining room"));
      };

      xmpp.on("muc:available", onMucPresence);
      xmpp.on("disconnected", onDisconnected);
    });
  }

  // -- Send delegators --

  private async requireConnectedXmpp(): Promise<Agent> {
    await this.connect();
    if (!this.xmpp || !this.connected || this.destroying) {
      throw new Error("XMPP session is not ready");
    }
    return this.xmpp;
  }

  private async requireJoinedRoom(w: string, c: string): Promise<{ xmpp: Agent; roomJid: string }> {
    const roomJid = roomBareJidFor(this.session, w, c);
    await this.switchRoom(w, c);
    if (!this.xmpp || !this.connected || this.destroying || this.currentRoom !== roomJid) {
      throw new Error(`Room is not ready: ${roomJid}`);
    }
    return { xmpp: this.xmpp, roomJid };
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

  private shouldQueueImmediately(): boolean {
    return this.browserOffline() || this.destroying || (!!this.xmpp && !this.connected);
  }

  private noteQueuedMessage() {
    const queueCount = countQueuedMessages(this.queueScope);
    const detail = queueCount === 1
      ? "Message queued until the connection returns"
      : `${queueCount} messages queued until the connection returns`;
    this.statusHandler?.({
      state: this.browserOffline() ? "offline" : "reconnecting",
      detail,
    });
  }

  private queueRoomMessage(
    roomJid: string,
    body: string,
    opts: messaging.SendGroupMessageOptions,
  ): OutboundSendResult {
    const queuedId = opts.id ?? globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
    enqueueQueuedMessage(this.queueScope, {
      kind: "room",
      id: queuedId,
      createdAt: new Date().toISOString(),
      roomJid,
      body,
      ...(opts.markup && opts.markup.length > 0 ? { markup: opts.markup } : {}),
      ...(opts.files && opts.files.length > 0 ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
      ...(opts.threadCreate ? { threadCreate: opts.threadCreate } : {}),
      ...(opts.threadReply ? { threadReply: opts.threadReply } : {}),
    });
    this.queuedMessageStatusHandler?.(queuedId, "queued");
    this.noteQueuedMessage();
    return { id: queuedId, state: "queued" };
  }

  private queueDirectMessage(
    peerJid: string,
    body: string,
    opts: dmMessaging.SendDirectMessageOptions,
  ): OutboundSendResult {
    const queuedId = opts.id ?? globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
    enqueueQueuedMessage(this.queueScope, {
      kind: "dm",
      id: queuedId,
      createdAt: new Date().toISOString(),
      peerJid: barePeerJid(peerJid),
      body,
      ...(opts.files && opts.files.length > 0 ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
    });
    this.queuedMessageStatusHandler?.(queuedId, "queued");
    this.noteQueuedMessage();
    return { id: queuedId, state: "queued" };
  }

  private nudgeReconnect() {
    void this.connect().catch(() => undefined);
  }

  private async flushQueuedDirectMessages() {
    if (this.directQueueFlushPromise) return this.directQueueFlushPromise;
    if (!this.canUseConnectedSession() || !this.xmpp) return;

    const flushPromise = (async () => {
      const entries = listQueuedMessages(this.queueScope).filter(
        (entry): entry is PersistedQueuedDmMessage => entry.kind === "dm",
      );
      for (const entry of entries) {
        if (!this.canUseConnectedSession() || !this.xmpp) break;
        this.queuedMessageStatusHandler?.(entry.id, "sending");
        const messageId = dmMessaging.sendDirectMessage(
          this.xmpp,
          barePeerJid(entry.peerJid),
          entry.body,
          {
            ...(entry.files && entry.files.length > 0 ? { files: entry.files } : {}),
            ...(entry.replyTo ? { replyTo: entry.replyTo } : {}),
            ...(entry.threadId ? { threadId: entry.threadId } : {}),
            ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}),
            id: entry.id,
          },
        );
        if (messageId) removeQueuedMessage(this.queueScope, entry.id);
      }
    })();
    const trackedPromise = flushPromise.finally(() => {
      if (this.directQueueFlushPromise === trackedPromise) {
        this.directQueueFlushPromise = null;
      }
    });
    this.directQueueFlushPromise = trackedPromise;
    return trackedPromise;
  }

  private async flushQueuedRoomMessages(roomJid: string) {
    const inFlight = this.roomQueueFlushes.get(roomJid);
    if (inFlight) return inFlight;
    if (!this.roomIsReady(roomJid) || !this.xmpp) return;

    const flushPromise = (async () => {
      const entries = listQueuedRoomMessages(this.queueScope, roomJid);
      for (const entry of entries) {
        if (!this.roomIsReady(roomJid) || !this.xmpp) break;
        this.queuedMessageStatusHandler?.(entry.id, "sending");
        const messageId = messaging.sendGroupMessage(this.xmpp, roomJid, entry.body, {
          ...(entry.markup && entry.markup.length > 0 ? { markup: entry.markup } : {}),
          ...(entry.files && entry.files.length > 0 ? { files: entry.files } : {}),
          ...(entry.replyTo ? { replyTo: entry.replyTo } : {}),
          ...(entry.threadId ? { threadId: entry.threadId } : {}),
          ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}),
          ...(entry.threadCreate ? { threadCreate: entry.threadCreate } : {}),
          ...(entry.threadReply ? { threadReply: entry.threadReply } : {}),
          id: entry.id,
        });
        if (messageId) removeQueuedMessage(this.queueScope, entry.id);
      }
    })();

    const trackedPromise = flushPromise.finally(() => {
      if (this.roomQueueFlushes.get(roomJid) === trackedPromise) {
        this.roomQueueFlushes.delete(roomJid);
      }
    });
    this.roomQueueFlushes.set(roomJid, trackedPromise);
    return trackedPromise;
  }

  async sendChatState(w: string, c: string, state: ChatStateType) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(w, c);
    messaging.sendChatState(xmpp, roomJid, state);
  }
  async sendDisplayed(w: string, c: string, messageId: string) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(w, c);
    messaging.sendDisplayed(xmpp, roomJid, messageId);
  }
  async sendReaction(w: string, c: string, messageId: string, emojis: string[]) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(w, c);
    messaging.sendReaction(xmpp, roomJid, messageId, emojis);
  }
  async sendRetraction(w: string, c: string, retractsId: string) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(w, c);
    messaging.sendRetraction(xmpp, roomJid, retractsId);
  }
  async sendModeration(w: string, c: string, targetId: string, reason?: string) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(w, c);
    messaging.sendModeration(xmpp, roomJid, targetId, reason);
  }
  async sendCorrection(w: string, c: string, body: string, replacesId: string, markup?: import("@/lib/chat-ui").MarkupSpan[]): Promise<string | null> {
    const { xmpp, roomJid } = await this.requireJoinedRoom(w, c);
    return messaging.sendCorrection(xmpp, roomJid, body, replacesId, markup);
  }
  async sendGroupMessage(
    w: string,
    c: string,
    body: string,
    opts: messaging.SendGroupMessageOptions = {},
  ): Promise<OutboundSendResult | null> {
    const { files } = opts;
    const text = body.trim();
    const hasFiles = !!files && files.length > 0;
    if (!text && !hasFiles) return null;

    const roomJid = roomBareJidFor(this.session, w, c);
    if (this.shouldQueueImmediately()) {
      const queued = this.queueRoomMessage(roomJid, body, opts);
      void this.connect()
        .then(() => this.switchRoom(w, c))
        .then(() => this.flushQueuedRoomMessages(roomJid))
        .catch(() => undefined);
      return queued;
    }

    try {
      const { xmpp } = await this.requireJoinedRoom(w, c);
      return {
        id: messaging.sendGroupMessage(xmpp, roomJid, body, opts),
        state: "sending",
      };
    } catch (error) {
      if (isLocalAvailabilityError(error)) {
        const queued = this.queueRoomMessage(roomJid, body, opts);
        void this.connect()
          .then(() => this.switchRoom(w, c))
          .then(() => this.flushQueuedRoomMessages(roomJid))
          .catch(() => undefined);
        return queued;
      }
      throw error;
    }
  }

  // -- File upload (XEP-0363 + XEP-0447/XEP-0448) --

  private async resolveUploadService(): Promise<string> {
    if (this.uploadServiceJid) return this.uploadServiceJid;
    await this.connect();
    if (!this.xmpp) throw new Error("XMPP not connected");
    const domain = jidDomain(this.session.jid);
    const jid = await discoverUploadService(this.xmpp, domain);
    if (!jid) throw new Error(`File upload service not available (domain: ${domain})`);
    this.uploadServiceJid = jid;
    return jid;
  }

  /** Encrypt, upload, and return attachment metadata ready for outbound send. */
  async uploadAttachments(
    files: ReadonlyArray<File | Blob>,
    onProgress?: (overall: UploadProgress, index: number) => void,
  ): Promise<messaging.OutboundFileAttachment[]> {
    await this.connect();
    if (!this.xmpp) throw new Error("XMPP not connected");
    const uploadDomain = await this.resolveUploadService();
    const preparedUploads = await Promise.all(files.map((file) => prepareEncryptedAttachmentUpload(file)));
    const totals = preparedUploads.map((prepared) => prepared.uploadFile.size);
    const loaded = files.map(() => 0);
    const grandTotal = totals.reduce((a, b) => a + b, 0);
    const results: messaging.OutboundFileAttachment[] = [];
    for (let i = 0; i < preparedUploads.length; i++) {
      const prepared = preparedUploads[i];
      const result = await uploadFile(this.xmpp, prepared.uploadFile, uploadDomain, (p) => {
        loaded[i] = p.loaded;
        const loadedSum = loaded.reduce((a, b) => a + b, 0);
        onProgress?.({ loaded: loadedSum, total: grandTotal }, i);
      });
      results.push({
        url: result.getUrl,
        name: prepared.originalName,
        mediaType: prepared.originalMediaType,
        size: prepared.originalSize,
        encrypted: {
          ...prepared.encrypted,
          sources: [result.getUrl],
        },
      });
    }
    return results;
  }

  async sendDirectMessage(
    peerJid: string,
    body: string,
    opts: dmMessaging.SendDirectMessageOptions = {},
  ): Promise<OutboundSendResult | null> {
    const { files } = opts;
    const text = body.trim();
    const hasFiles = !!files && files.length > 0;
    if (!text && !hasFiles) return null;

    const normalizedPeerJid = barePeerJid(peerJid);
    if (this.shouldQueueImmediately()) {
      const queued = this.queueDirectMessage(normalizedPeerJid, body, opts);
      this.nudgeReconnect();
      return queued;
    }

    try {
      const xmpp = await this.requireConnectedXmpp();
      return {
        id: dmMessaging.sendDirectMessage(xmpp, normalizedPeerJid, body, opts),
        state: "sending",
      };
    } catch (error) {
      if (isLocalAvailabilityError(error)) {
        const queued = this.queueDirectMessage(normalizedPeerJid, body, opts);
        this.nudgeReconnect();
        return queued;
      }
      throw error;
    }
  }

  async sendDmChatState(peerJid: string, state: ChatStateType): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    dmMessaging.sendDmChatState(xmpp, barePeerJid(peerJid), state);
  }

  async sendDmDisplayed(peerJid: string, messageId: string): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    dmMessaging.sendDmDisplayed(xmpp, barePeerJid(peerJid), messageId);
  }

  async sendDmRetraction(peerJid: string, messageId: string): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    dmMessaging.sendDmRetraction(xmpp, barePeerJid(peerJid), messageId);
  }

  async sendDmCorrection(peerJid: string, body: string, replacesId: string): Promise<string | null> {
    const xmpp = await this.requireConnectedXmpp();
    return dmMessaging.sendDmCorrection(xmpp, barePeerJid(peerJid), body, replacesId);
  }

  async sendDmReaction(peerJid: string, messageId: string, emojis: string[]): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    dmMessaging.sendDmReaction(xmpp, barePeerJid(peerJid), messageId, emojis);
  }

  async enablePushNotifications(opts: {
    serviceJid: string;
    node?: string;
    endpoint: string;
    p256dh: string;
    auth: string;
  }): Promise<boolean> {
    await this.connect();
    if (!this.xmpp) return false;
    try {
      await this.xmpp.sendIQ({
        type: "set",
        pushEnable: {
          jid: opts.serviceJid,
          node: opts.node ?? "web-push",
          endpoint: opts.endpoint,
          p256dh: opts.p256dh,
          auth: opts.auth,
        },
      } as Parameters<Agent["sendIQ"]>[0]);
      return true;
    } catch (err) {
      console.warn("Failed to enable XMPP push notifications", err);
      return false;
    }
  }

  async disablePushNotifications(opts: {
    serviceJid: string;
    node?: string;
  }): Promise<boolean> {
    await this.connect();
    if (!this.xmpp) return false;
    try {
      await this.xmpp.sendIQ({
        type: "set",
        pushDisable: {
          jid: opts.serviceJid,
          node: opts.node ?? "web-push",
        },
      } as Parameters<Agent["sendIQ"]>[0]);
      return true;
    } catch (err) {
      console.warn("Failed to disable XMPP push notifications", err);
      return false;
    }
  }

  // -- Inbox (XEP-0430) --

  async fetchInbox(opts: inboxApi.FetchInboxOptions = {}): Promise<inboxApi.InboxResult> {
    await this.connect();
    if (!this.xmpp) return { totalUnread: 0, conversations: [] };
    return inboxApi.fetchInbox(this.xmpp, opts);
  }

  async markInboxRead(partnerJid: string, threadId?: string): Promise<void> {
    await this.connect();
    if (this.xmpp) await inboxApi.markInboxRead(this.xmpp, barePeerJid(partnerJid), threadId);
  }

  async fetchThreadInbox(roomJid: string): Promise<inboxApi.InboxResult> {
    await this.connect();
    if (!this.xmpp) return { totalUnread: 0, conversations: [] };
    return inboxApi.fetchInbox(this.xmpp, { room: roomJid, threads: true });
  }

  // -- PEP Mood / Activity / Tune (XEP-0107 / 0108 / 0118) --

  async publishMood(mood: pep.MoodPublication): Promise<void> {
    await this.connect();
    if (this.xmpp) await pep.publishMood(this.xmpp, mood);
  }
  async retractMood(): Promise<void> {
    await this.connect();
    if (this.xmpp) await pep.retractMood(this.xmpp);
  }
  async publishActivity(activity: pep.ActivityPublication): Promise<void> {
    await this.connect();
    if (this.xmpp) await pep.publishActivity(this.xmpp, activity);
  }
  async retractActivity(): Promise<void> {
    await this.connect();
    if (this.xmpp) await pep.retractActivity(this.xmpp);
  }
  async publishTune(tune: pep.TunePublication): Promise<void> {
    await this.connect();
    if (this.xmpp) await pep.publishTune(this.xmpp, tune);
  }
  async retractTune(): Promise<void> {
    await this.connect();
    if (this.xmpp) await pep.retractTune(this.xmpp);
  }

  async fetchUserPepProfile(jid: string): Promise<pep.UserPepProfile> {
    await this.connect();
    if (!this.xmpp) return { mood: null, activity: null, tune: null };
    return pep.fetchUserPepProfile(this.xmpp, jid);
  }

  // -- Query delegators --

  async queryMam(w: string, c: string, max = 50): Promise<LiveRoomMessage[]> {
    await this.connect(); await this.switchRoom(w, c);
    return this.xmpp ? history.queryMam(this.xmpp, roomBareJidFor(this.session, w, c), max) : [];
  }
  async queryMamByThread(w: string, c: string, threadId: string, max = 100): Promise<LiveRoomMessage[]> {
    await this.connect(); await this.switchRoom(w, c);
    return this.xmpp
      ? history.queryMamByThread(this.xmpp, roomBareJidFor(this.session, w, c), threadId, max)
      : [];
  }
  async searchMessages(w: string, c: string, query: string, max = 20) {
    await this.connect();
    return this.xmpp ? history.searchMessages(this.xmpp, roomBareJidFor(this.session, w, c), query, max) : [];
  }
  async queryPersonalMam(peerJid: string, max = 100): Promise<LiveDmMessage[]> {
    await this.connect();
    const selfBare = barePeerJid(this.session.jid);
    return this.xmpp ? dmHistory.queryPersonalMam(this.xmpp, selfBare, barePeerJid(peerJid), max) : [];
  }
  async searchDmMessages(peerJid: string, query: string, max = 20) {
    await this.connect();
    const selfBare = barePeerJid(this.session.jid);
    return this.xmpp ? dmHistory.searchDmMessages(this.xmpp, selfBare, barePeerJid(peerJid), query, max) : [];
  }
  async subscribeToPeerPresence(peerJid: string): Promise<void> {
    await this.connect();
    this.xmpp?.subscribe(barePeerJid(peerJid));
  }
  async discoverWaddles(): Promise<DiscoveredWaddle[]> {
    await this.connect();
    return this.xmpp
      ? discovery.discoverWaddles(this.xmpp, this.session.jid)
      : [];
  }

  /**
   * XEP-0092 Software Version — ask the user's home server what it is running.
   * Returns null if the query fails (e.g. federation, timeout, feature not
   * supported by a third-party server).
   */
  async getServerVersion(): Promise<{ name?: string; version?: string; os?: string } | null> {
    await this.connect();
    if (!this.xmpp) return null;
    const domain = jidDomain(this.session.jid);
    if (!domain) return null;
    try {
      return await this.xmpp.getSoftwareVersion(domain);
    } catch (err) {
      if (!isOptionalXmppFeatureError(err)) {
        console.warn("Failed to query XEP-0092 software version", err);
      }
      return null;
    }
  }
  async discoverChannels(waddleId: string): Promise<DiscoveredChannel[]> {
    await this.connect();
    return this.xmpp
      ? discovery.discoverChannels(this.xmpp, this.session.jid, waddleId)
      : [];
  }

  /**
   * Get the underlying XMPP agent for advanced operations like ad-hoc commands.
   * Returns null if not connected.
   */
  get agent(): Agent | null {
    return this.xmpp;
  }

  // -- Private --

  /**
   * XEP-0313 MAM catch-up after a `session:started`.
   *
   * Empty on the first session:started (initial login); for each subsequent
   * resume it queries MAM with `start=<lastSeen>` for every tracked
   * conversation and feeds the results through the same handlers as live
   * messages. This closes the gap when mobile Safari (or any suspended tab)
   * silently drops its WebSocket and reconnects after missing stanzas.
   */
  private async runReconnectCatchup(xmpp: Agent) {
    const entries = this.catchup.onSessionStarted();
    if (entries.length === 0) return;

    const selfBare = barePeerJid(this.session.jid);
    for (const entry of entries) {
      // Bail if the agent has been swapped out mid-catch-up (e.g. user
      // signed out during reconnect).
      if (this.xmpp !== xmpp) return;
      try {
        if (entry.kind === "dm") {
          const since = this.catchup.getDmLastSeen(entry.key);
          const messages = await dmHistory.queryPersonalMam(
            xmpp,
            selfBare,
            entry.key,
            CATCHUP_MAX_PER_CONVERSATION,
            since,
          );
          for (const m of messages) {
            this.catchup.recordDmSeen(m.peerJid, m.createdAt);
            this.directMessageHandler?.(m);
          }
        } else {
          const since = this.catchup.getRoomLastSeen(entry.key);
          const messages = await history.queryMam(
            xmpp,
            entry.key,
            CATCHUP_MAX_PER_CONVERSATION,
            since,
          );
          for (const m of messages) {
            this.catchup.recordRoomSeen(m.roomJid, m.createdAt);
            this.messageHandler?.(m);
          }
        }
      } catch (error) {
        if (!isOptionalXmppFeatureError(error)) {
          console.warn(
            `MAM catch-up failed for ${entry.kind}:${entry.key}`,
            error,
          );
        }
      }
    }
  }

  private startSelfPing() {
    this.stopSelfPing();
    this.selfPingTimer = setInterval(() => { void this.doSelfPing(); }, 60_000);
  }
  private stopSelfPing() {
    if (this.selfPingTimer) { clearInterval(this.selfPingTimer); this.selfPingTimer = null; }
  }
  private async doSelfPing() {
    if (!this.xmpp || !this.currentRoom) return;
    try { await this.xmpp.sendIQ({ type: "get", to: `${this.currentRoom}/${this.session.username}`, ping: true }); }
    catch { this.roomDisconnectHandler?.(); }
  }

  /**
   * Keep the websocket alive through stanza's own SM-aware keepalive. When
   * stream management is active stanza requests an SM ack instead of issuing a
   * separate XMPP ping, and on timeout it drops the transport without marking
   * the session as intentionally disconnected.
   */
  private startKeepAlive(xmpp: Agent) {
    this.stopKeepAlive();
    xmpp.enableKeepAlive({ interval: 30, timeout: 15 });
  }

  private stopKeepAlive() {
    this.xmpp?.disableKeepAlive();
  }

  /**
   * Listen for "tab became visible" / "network back online" for the lifetime
   * of the agent. When fired, connect() no-ops if already online; otherwise it
   * actively drives reconnection so recovery doesn't rely only on stanza.js's
   * internal backoff.
   */
  private startReconnectNudgeListeners() {
    this.stopReconnectNudgeListeners();
    const nudge = () => {
      const now = Date.now();
      if (now - this.reconnectNudgeAt < 1500) return;
      this.reconnectNudgeAt = now;
      if (this.destroying || !this.xmpp) return;
      void this.connect().catch(() => undefined);
    };
    if (typeof document !== "undefined") {
      this.visibilityListener = () => {
        if (document.visibilityState === "visible") nudge();
      };
      document.addEventListener("visibilitychange", this.visibilityListener);
    }
    if (typeof window !== "undefined") {
      this.onlineListener = nudge;
      window.addEventListener("online", this.onlineListener);
    }
  }

  private stopReconnectNudgeListeners() {
    if (this.visibilityListener && typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", this.visibilityListener);
    }
    this.visibilityListener = null;
    if (this.onlineListener && typeof window !== "undefined") {
      window.removeEventListener("online", this.onlineListener);
    }
    this.onlineListener = null;
  }

  private wireEvents(xmpp: Agent) {
    xmpp.on("session:started", async () => {
      if (this.xmpp !== xmpp) return;
      this.connected = true;
      this.startKeepAlive(xmpp);
      this.statusHandler?.({
        state: "online",
        detail: countQueuedMessages(this.queueScope) > 0 ? "Reconnected — replaying queued messages" : "Connection ready",
      });
      // `session:started` fires on fresh bind only; SM resume short-circuits
      // feature negotiation and does not emit this event. So any time we see
      // it here, it's a fresh session — consumers may close MAM gaps.
      this.sessionLifecycleHandler?.({ type: "fresh" });
      void this.flushQueuedDirectMessages();
      try {
        await xmpp.enableCarbons();
      } catch (error) {
        if (!isOptionalXmppFeatureError(error)) {
          console.warn("Failed to enable message carbons", error);
        }
      }
      try {
        const roster = await xmpp.getRoster();
        for (const item of roster.items ?? []) {
          if (item.jid) xmpp.subscribe(item.jid);
        }
      } catch (error) {
        if (!isOptionalXmppFeatureError(error)) {
          console.warn("Failed to refresh roster presence subscriptions", error);
        }
      }

      // Rejoin room if we were in one before the disconnect
      const roomToRejoin = this.currentRoom;
      if (roomToRejoin) {
        this.currentRoom = null;
        this.roomHats = {};
        this.hatsHandler?.({});
        this.roomPresence = {};
        this.presenceHandler?.({});
        try {
          await this.performRoomSwitch(roomToRejoin);
        } catch (error) {
          console.warn("Failed to rejoin room after reconnect", error);
        }
      }

      // Slice A: MAM catch-up for every tracked conversation. Empty on the
      // first session:started (initial login — nothing missed), populated on
      // subsequent ones (resume after a dropped socket or Safari backgrounding).
      await this.runReconnectCatchup(xmpp);
    });
    xmpp.on("stream:management:resumed", () => {
      if (this.xmpp !== xmpp) return;
      this.connected = true;
      this.startKeepAlive(xmpp);
      this.statusHandler?.({
        state: "online",
        detail: countQueuedMessages(this.queueScope) > 0 ? "Connection resumed — replaying queued messages" : "Connection resumed",
      });
      this.sessionLifecycleHandler?.({ type: "resumed" });
      void this.flushQueuedDirectMessages();
      if (this.currentRoom) {
        void this.flushQueuedRoomMessages(this.currentRoom);
      }
    });

    // XEP-0198: per-stanza ack from the server. stanza.js resolves individual
    // stanzas against the ack count and emits message:acked with the original
    // outbound Message. Downstream uses stanza.id to move the optimistic
    // "sending" entry to "delivered".
    xmpp.on("message:acked", (msg) => {
      if (this.xmpp !== xmpp) return;
      if (msg?.id) this.messageAckHandler?.(msg.id);
    });

    // XEP-0198: stanza.js emits message:failed when SM resume fails (server
    // drops the session) or when we tried to send with no transport and SM is
    // not resumable. The message was almost certainly not delivered.
    xmpp.on("message:failed", (msg) => {
      if (this.xmpp !== xmpp) return;
      if (msg?.id) this.messageDeliveryFailureHandler?.(msg.id);
    });

    xmpp.on("disconnected", (err) => {
      if (this.xmpp !== xmpp) return;

      this.connected = false;
      this.connectPromise = null;
      this.stopSelfPing();
      this.stopKeepAlive();

      if (this.destroying) {
        this.stopReconnectNudgeListeners();
        this.xmpp = null;
        this.currentRoom = null;
        this.roomSwitchPromise = null;
        this.roomSwitchTarget = null;
        this.roomHats = {};
        this.hatsHandler?.({});
        this.roomPresence = {};
        this.presenceHandler?.({});
        this.statusHandler?.({ state: "offline", detail: err?.message ?? "Disconnected" });
      } else {
        // Unexpected drop — keep xmpp + currentRoom alive for auto-reconnect.
        this.statusHandler?.({
          state: "reconnecting",
          detail: countQueuedMessages(this.queueScope) > 0
            ? "Connection lost — queued messages will send when reconnected"
            : (err?.message ?? "Connection lost, reconnecting..."),
        });
      }
      console.error("XMPP disconnected", err);
    });
    xmpp.on("stream:error", (streamError, err) => {
      const condition = streamError?.condition;
      const fatal = condition === "not-authorized" || condition === "host-unknown" || condition === "host-gone";
      if (fatal) {
        this.destroying = true;
      }
      const detail = err?.message ?? condition ?? "Stream error";
      this.statusHandler?.({ state: fatal ? "error" : "reconnecting", detail });
      console.error("XMPP stream error", detail);
    });
    xmpp.on("auth:failed" as any, async () => {
      if (this.xmpp !== xmpp) return;
      if (!this.refreshSession) {
        this.destroying = true;
        this.statusHandler?.({ state: "error", detail: "Session expired. Please log in again." });
        return;
      }
      try {
        const refreshed = await this.refreshSession();
        if (refreshed) {
          this.updateSession(refreshed, xmpp);
        } else {
          this.destroying = true;
          this.statusHandler?.({ state: "error", detail: "Session expired. Please log in again." });
        }
      } catch {
        this.destroying = true;
        this.statusHandler?.({ state: "error", detail: "Session expired. Please log in again." });
      }
    });

    xmpp.on("muc:available", (pres: ReceivedMUCPresence) => {
      try {
        const [room, nick] = (pres.from ?? "").split("/");
        if (room === this.currentRoom && nick) {
          this.roomHats[nick] = parsePresenceHats(ext(pres).hats ?? ext(pres).hat);
          this.hatsHandler?.({ ...this.roomHats });
          this.roomPresence[nick] = parsePresenceShow(pres.show);
          this.presenceHandler?.({ ...this.roomPresence });
          const item = (ext(pres).muc as { item?: { jid?: string } } | undefined)?.item;
          if (item?.jid) {
            this.memberJidHandler?.(nick, barePeerJid(item.jid));
          }
        }
        if (room && !nick && typeof pres.vcardAvatar === "string" && pres.vcardAvatar)
          this.roomAvatarHandler?.(room, pres.vcardAvatar);
      } catch (error) {
        console.error("Failed to process MUC presence", error);
      }
    });

    xmpp.on("muc:unavailable", (pres: ReceivedMUCPresence) => {
      try {
        const [room, nick] = (pres.from ?? "").split("/");
        if (room === this.currentRoom && nick) {
          delete this.roomHats[nick];
          this.hatsHandler?.({ ...this.roomHats });
          this.roomPresence[nick] = "offline";
          this.presenceHandler?.({ ...this.roomPresence });
          this.lastSeenHandler?.(nick, Date.now());
        }
      } catch (error) {
        console.error("Failed to process MUC unavailable presence", error);
      }
    });

    xmpp.on("message:error", (msg) => {
      if (msg.error?.condition === "policy-violation") {
        const s = parseInt(msg.error.text?.match(/wait\s+(\d+)/i)?.[1] ?? "0", 10);
        if (s > 0) this.slowModeHandler?.(s);
      }
    });

    xmpp.on("message", buildMessageDispatcher(
      () => ({
        currentRoom: this.currentRoom,
        selfNick: this.session.username,
        onMessage: (m) => {
          this.catchup.recordRoomSeen(m.roomJid, m.createdAt);
          this.messageHandler?.(m);
        },
        onChatState: this.chatStateHandler,
        onDisplayed: this.displayedHandler,
        onReaction: this.reactionHandler,
        onActivity: this.activityHandler,
      }),
      () => ({
        selfBareJid: barePeerJid(this.session.jid),
        onMessage: (m) => {
          this.catchup.recordDmSeen(m.peerJid, m.createdAt);
          this.directMessageHandler?.(m);
        },
        onChatState: this.dmChatStateHandler,
        onDisplayed: this.dmDisplayedHandler,
        onReaction: this.dmReactionHandler,
      }),
    ));

    xmpp.on("carbon:sent", (msg) => {
      const forwarded = msg.carbon?.forward?.message;
      if (!forwarded) return;
      if (!forwarded.from || !forwarded.to) return;
      (forwarded as ReceivedMessage & { carbon?: unknown }).carbon = msg.carbon;
      dispatchChat(forwarded as ReceivedMessage, {
        selfBareJid: barePeerJid(this.session.jid),
        onMessage: (m) => {
          this.catchup.recordDmSeen(m.peerJid, m.createdAt);
          this.directMessageHandler?.(m);
        },
        onChatState: this.dmChatStateHandler,
        onDisplayed: this.dmDisplayedHandler,
        onReaction: this.dmReactionHandler,
      });
    });

    xmpp.on("message", (msg: ReceivedMessage) => {
      const push = (msg as ReceivedMessage & { inboxPush?: { partner?: string; kind?: string; lastStanzaId?: string; lastUpdated?: number; unread?: number; preview?: string } }).inboxPush;
      if (!push?.partner) return;
      this.inboxPushHandler?.({
        partner: push.partner,
        kind: push.kind === "muc" ? "muc" : "direct",
        lastStanzaId: push.lastStanzaId ?? "",
        lastUpdated: typeof push.lastUpdated === "number" ? push.lastUpdated : 0,
        unread: typeof push.unread === "number" ? push.unread : 0,
        preview: push.preview || undefined,
      });
    });

    xmpp.on("presence", (pres: ReceivedPresence) => {
      const from = barePeerJid(pres.from ?? "");
      if (!from) return;
      const selfDomain = barePeerJid(this.session.jid).split("@")[1] ?? "";
      if (selfDomain && from.endsWith(`@muc.${selfDomain}`)) return;

      if (pres.type === "subscribe") {
        if (from !== barePeerJid(this.session.jid)) {
          xmpp.acceptSubscription(from);
          xmpp.subscribe(from);
        }
        return;
      }

      this.presenceUpdateHandler?.({
        bareJid: from,
        show: mapPresenceShow(pres),
        status: pres.status,
      });
    });
  }
}

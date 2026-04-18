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
import { barePeerJid, roomBareJidFor } from "./jid";
import { registerWaddleExtensions } from "./extensions";
import { dispatchGroupchat, ext } from "./message-parsing";
import { dispatchChat } from "./dm-parsing";
import * as messaging from "./messaging";
import * as history from "./history";
import * as dmMessaging from "./dm-messaging";
import * as dmHistory from "./dm-history";
import * as discovery from "./discovery";
import { discoverUploadService, uploadFile, type UploadProgress } from "./file-upload";

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

export class BrowserXmppClient {
  private readonly session: WaddleSession;
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
  private roomAvatarHandler: ((roomJid: string, hash: string) => void) | null = null;
  private roomDisconnectHandler: (() => void) | null = null;
  private presenceHandler: ((presence: RoomPresence) => void) | null = null;
  private lastSeenHandler: ((nick: string, timestamp: number) => void) | null = null;
  private messageAckHandler: ((messageId: string) => void) | null = null;
  private messageDeliveryFailureHandler: ((messageId: string) => void) | null = null;
  private sessionLifecycleHandler: ((event: SessionLifecycleEvent) => void) | null = null;
  private xmpp: Agent | null = null;
  private connectPromise: Promise<void> | null = null;
  private connected = false;
  private destroying = false;
  private refreshSession: (() => Promise<WaddleSession | null>) | null = null;
  private currentRoom: string | null = null;
  private roomSwitchPromise: Promise<void> | null = null;
  private roomSwitchTarget: string | null = null;
  private selfPingTimer: ReturnType<typeof setInterval> | null = null;
  private keepAliveTimer: ReturnType<typeof setInterval> | null = null;
  private visibilityListener: (() => void) | null = null;
  private onlineListener: (() => void) | null = null;
  private roomHats: RoomHats = {};
  private roomPresence: RoomPresence = {};
  private uploadServiceJid: string | null = null;

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
  setRoomAvatarHandler(h: (roomJid: string, hash: string) => void) { this.roomAvatarHandler = h; }
  setRoomDisconnectHandler(h: () => void) { this.roomDisconnectHandler = h; }
  setPresenceHandler(h: (presence: RoomPresence) => void) { this.presenceHandler = h; }
  setLastSeenHandler(h: (nick: string, timestamp: number) => void) { this.lastSeenHandler = h; }
  setMessageAckHandler(h: (messageId: string) => void) { this.messageAckHandler = h; }
  setMessageDeliveryFailureHandler(h: (messageId: string) => void) { this.messageDeliveryFailureHandler = h; }
  setSessionLifecycleHandler(h: (event: SessionLifecycleEvent) => void) { this.sessionLifecycleHandler = h; }
  setRefreshSession(fn: () => Promise<WaddleSession | null>) { this.refreshSession = fn; }

  // -- Connection lifecycle --

  async connect() {
    if (this.xmpp && this.connected) return;
    if (this.connectPromise) return this.connectPromise;

    // If the Agent exists but is disconnected, Stanza's autoReconnect is
    // handling backoff — wait for either a fresh bind (session:started) or an
    // XEP-0198 resume (stream:management:resumed); resume does NOT emit
    // session:started because feature negotiation is short-circuited.
    if (this.xmpp && !this.destroying) {
      const xmpp = this.xmpp;
      this.connectPromise = new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => {
          cleanup();
          this.connectPromise = null;
          reject(new Error("Reconnection timed out"));
        }, 60_000);
        const cleanup = () => {
          clearTimeout(timeout);
          xmpp.off("session:started", onReady);
          xmpp.off("stream:management:resumed", onReady);
        };
        const onReady = () => { cleanup(); this.connectPromise = null; resolve(); };
        xmpp.on("session:started", onReady);
        xmpp.on("stream:management:resumed", onReady);
      });
      return this.connectPromise;
    }

    // Fresh connection — tear down any stale agent
    if (this.xmpp) {
      try { this.xmpp.disconnect(); } catch { /* ignore stale client */ }
      this.xmpp = null;
    }
    this.destroying = false;
    this.currentRoom = null;
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
    const xmpp = this.xmpp;
    this.xmpp = null;
    this.connectPromise = null;
    this.connected = false;
    this.currentRoom = null;
    this.uploadServiceJid = null;
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

  async sendChatState(w: string, c: string, state: ChatStateType) {
    if (this.xmpp) messaging.sendChatState(this.xmpp, roomBareJidFor(this.session, w, c), state);
  }
  async sendDisplayed(w: string, c: string, messageId: string) {
    if (this.xmpp) messaging.sendDisplayed(this.xmpp, roomBareJidFor(this.session, w, c), messageId);
  }
  async sendReaction(w: string, c: string, messageId: string, emojis: string[]) {
    if (this.xmpp) messaging.sendReaction(this.xmpp, roomBareJidFor(this.session, w, c), messageId, emojis);
  }
  async sendRetraction(w: string, c: string, retractsId: string) {
    if (this.xmpp) messaging.sendRetraction(this.xmpp, roomBareJidFor(this.session, w, c), retractsId);
  }
  async sendModeration(w: string, c: string, targetId: string, reason?: string) {
    if (this.xmpp) messaging.sendModeration(this.xmpp, roomBareJidFor(this.session, w, c), targetId, reason);
  }
  async sendCorrection(w: string, c: string, body: string, replacesId: string, markup?: import("@/lib/chat-ui").MarkupSpan[]): Promise<string | null> {
    return this.xmpp ? messaging.sendCorrection(this.xmpp, roomBareJidFor(this.session, w, c), body, replacesId, markup) : null;
  }
  async sendGroupMessage(w: string, c: string, body: string, markup?: import("@/lib/chat-ui").MarkupSpan[]): Promise<string | null> {
    await this.connect(); await this.switchRoom(w, c);
    return this.xmpp ? messaging.sendGroupMessage(this.xmpp, roomBareJidFor(this.session, w, c), body, markup) : null;
  }

  // -- File upload (XEP-0363 + XEP-0447) --

  private async resolveUploadService(): Promise<string> {
    if (this.uploadServiceJid) return this.uploadServiceJid;
    await this.connect();
    if (!this.xmpp) throw new Error("XMPP not connected");
    const domain = this.session.jid.split("@")[1] ?? "localhost";
    const jid = await discoverUploadService(this.xmpp, domain);
    if (!jid) throw new Error(`File upload service not available (domain: ${domain})`);
    this.uploadServiceJid = jid;
    return jid;
  }

  async uploadAndSendGroupFile(
    w: string,
    c: string,
    file: File | Blob,
    onProgress?: (progress: UploadProgress) => void,
  ): Promise<{ msgId: string; fileUrl: string } | null> {
    await this.connect();
    await this.switchRoom(w, c);
    if (!this.xmpp) return null;
    const uploadDomain = await this.resolveUploadService();
    const result = await uploadFile(this.xmpp, file, uploadDomain, onProgress);
    const msgId = messaging.sendGroupFileMessage(
      this.xmpp,
      roomBareJidFor(this.session, w, c),
      result.getUrl,
      { name: result.filename, mediaType: result.contentType, size: result.size },
    );
    return { msgId, fileUrl: result.getUrl };
  }

  async uploadAndSendDirectFile(
    peerJid: string,
    file: File | Blob,
    onProgress?: (progress: UploadProgress) => void,
  ): Promise<{ msgId: string; fileUrl: string } | null> {
    await this.connect();
    if (!this.xmpp) return null;
    const uploadDomain = await this.resolveUploadService();
    const result = await uploadFile(this.xmpp, file, uploadDomain, onProgress);
    const msgId = dmMessaging.sendDirectFileMessage(
      this.xmpp,
      barePeerJid(peerJid),
      result.getUrl,
      { name: result.filename, mediaType: result.contentType, size: result.size },
    );
    return { msgId, fileUrl: result.getUrl };
  }

  async sendDirectMessage(peerJid: string, body: string): Promise<string | null> {
    await this.connect();
    return this.xmpp ? dmMessaging.sendDirectMessage(this.xmpp, barePeerJid(peerJid), body) : null;
  }

  async sendDmChatState(peerJid: string, state: ChatStateType): Promise<void> {
    await this.connect();
    if (this.xmpp) dmMessaging.sendDmChatState(this.xmpp, barePeerJid(peerJid), state);
  }

  async sendDmDisplayed(peerJid: string, messageId: string): Promise<void> {
    await this.connect();
    if (this.xmpp) dmMessaging.sendDmDisplayed(this.xmpp, barePeerJid(peerJid), messageId);
  }

  async sendDmRetraction(peerJid: string, messageId: string): Promise<void> {
    await this.connect();
    if (this.xmpp) dmMessaging.sendDmRetraction(this.xmpp, barePeerJid(peerJid), messageId);
  }

  async sendDmCorrection(peerJid: string, body: string, replacesId: string): Promise<string | null> {
    await this.connect();
    return this.xmpp ? dmMessaging.sendDmCorrection(this.xmpp, barePeerJid(peerJid), body, replacesId) : null;
  }

  async sendDmReaction(peerJid: string, messageId: string, emojis: string[]): Promise<void> {
    await this.connect();
    if (this.xmpp) dmMessaging.sendDmReaction(this.xmpp, barePeerJid(peerJid), messageId, emojis);
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

  // -- Query delegators --

  async queryMam(w: string, c: string, max = 50): Promise<LiveRoomMessage[]> {
    await this.connect(); await this.switchRoom(w, c);
    return this.xmpp ? history.queryMam(this.xmpp, roomBareJidFor(this.session, w, c), max) : [];
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
  async discoverChannels(waddleId: string): Promise<DiscoveredChannel[]> {
    await this.connect();
    return this.xmpp
      ? discovery.discoverChannels(this.xmpp, this.session.jid, waddleId)
      : [];
  }

  // -- Private --

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
   * Keep the websocket alive in the background by pinging the server every
   * 30s. Also triggers an immediate ping when the tab becomes visible or the
   * browser regains network, so users who return to a backgrounded tab aren't
   * silently disconnected.
   */
  private startKeepAlive() {
    this.stopKeepAlive();
    this.keepAliveTimer = setInterval(() => { void this.pingServer(); }, 30_000);

    if (typeof document !== "undefined") {
      this.visibilityListener = () => {
        if (document.visibilityState === "visible") void this.pingServer();
      };
      document.addEventListener("visibilitychange", this.visibilityListener);
    }
    if (typeof window !== "undefined") {
      this.onlineListener = () => { void this.pingServer(); };
      window.addEventListener("online", this.onlineListener);
    }
  }

  private stopKeepAlive() {
    if (this.keepAliveTimer) {
      clearInterval(this.keepAliveTimer);
      this.keepAliveTimer = null;
    }
    if (this.visibilityListener && typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", this.visibilityListener);
      this.visibilityListener = null;
    }
    if (this.onlineListener && typeof window !== "undefined") {
      window.removeEventListener("online", this.onlineListener);
      this.onlineListener = null;
    }
  }

  private async pingServer() {
    const xmpp = this.xmpp;
    if (!xmpp || !this.connected || this.destroying) return;
    const domain = this.session.jid.split("@")[1];
    if (!domain) return;
    let timeoutHandle: ReturnType<typeof setTimeout> | null = null;
    try {
      const timeout = new Promise<never>((_, reject) => {
        timeoutHandle = setTimeout(() => reject(new Error("ping timeout")), 10_000);
      });
      await Promise.race([
        xmpp.sendIQ({ type: "get", to: domain, ping: true } as Parameters<Agent["sendIQ"]>[0]),
        timeout,
      ]);
    } catch (err) {
      // Server-side XMPP errors (e.g. service-unavailable) mean the connection
      // is alive — don't tear it down.
      if (xmppErrorCondition(err)) return;
      // Transport-level failure — drop the socket so autoReconnect re-opens it.
      if (this.xmpp !== xmpp) return;
      try { xmpp.disconnect(); } catch { /* ignore */ }
    } finally {
      if (timeoutHandle !== null) clearTimeout(timeoutHandle);
    }
  }

  private wireEvents(xmpp: Agent) {
    xmpp.on("session:started", async () => {
      if (this.xmpp !== xmpp) return;
      this.connected = true;
      this.startKeepAlive();
      this.statusHandler?.({ state: "online", detail: "Connection ready" });
      // `session:started` fires on fresh bind only; SM resume short-circuits
      // feature negotiation and does not emit this event. So any time we see
      // it here, it's a fresh session — consumers may close MAM gaps.
      this.sessionLifecycleHandler?.({ type: "fresh" });
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
    });
    xmpp.on("stream:management:resumed", () => {
      if (this.xmpp !== xmpp) return;
      this.connected = true;
      this.startKeepAlive();
      this.statusHandler?.({ state: "online", detail: "Connection resumed" });
      this.sessionLifecycleHandler?.({ type: "resumed" });
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
        // Intentional disconnect — full cleanup
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
        // Unexpected drop — keep xmpp + currentRoom alive for auto-reconnect
        this.statusHandler?.({ state: "reconnecting", detail: err?.message ?? "Connection lost, reconnecting..." });
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
          (xmpp.config as any).credentials = { token: refreshed.session_id };
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

    xmpp.on("groupchat", (msg) => dispatchGroupchat(msg, {
      currentRoom: this.currentRoom,
      selfNick: this.session.username,
      onMessage: this.messageHandler,
      onChatState: this.chatStateHandler,
      onDisplayed: this.displayedHandler,
      onReaction: this.reactionHandler,
      onActivity: this.activityHandler,
    }));

    xmpp.on("chat", (msg) => dispatchChat(msg, {
      selfBareJid: barePeerJid(this.session.jid),
      onMessage: this.directMessageHandler,
      onChatState: this.dmChatStateHandler,
      onDisplayed: this.dmDisplayedHandler,
      onReaction: this.dmReactionHandler,
    }));

    xmpp.on("carbon:sent", (msg) => {
      const forwarded = msg.carbon?.forward?.message;
      if (!forwarded) return;
      if (!forwarded.from || !forwarded.to) return;
      (forwarded as ReceivedMessage & { carbon?: unknown }).carbon = msg.carbon;
      dispatchChat(forwarded as ReceivedMessage, {
        selfBareJid: barePeerJid(this.session.jid),
        onMessage: this.directMessageHandler,
        onChatState: this.dmChatStateHandler,
        onDisplayed: this.dmDisplayedHandler,
        onReaction: this.dmReactionHandler,
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

/** BrowserXmppClient — thin orchestrator that delegates to functional modules. */
import { createClient } from "stanza";
import type { Agent } from "stanza";
import type { ReceivedMUCPresence } from "stanza/protocol";
import type { WaddleSession } from "../server-auth";
import type { WaddleHat } from "./extensions/hats";
import type {
  ChatStateEvent, ChatStateType, DiscoveredChannel, DiscoveredWaddle,
  DisplayedEvent, LiveRoomMessage, ReactionEvent, RoomActivityEvent, RoomHats, XmppStatusSnapshot,
} from "./types";
import { roomBareJidFor } from "./jid";
import { registerWaddleExtensions } from "./extensions";
import { dispatchGroupchat, ext } from "./message-parsing";
import * as messaging from "./messaging";
import * as history from "./history";
import * as discovery from "./discovery";

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

function parsePresenceHats(value: unknown): WaddleHat[] {
  const hats = Array.isArray(value) ? value : value ? [value] : [];
  return hats
    .map((hat) => hat as Partial<WaddleHat>)
    .map((hat) => ({ title: hat.title ?? "", uri: hat.uri ?? "" }))
    .filter((hat) => hat.title && hat.uri);
}

export class BrowserXmppClient {
  private readonly session: WaddleSession;
  private readonly resource = createXmppResource();
  private messageHandler: ((message: LiveRoomMessage) => void) | null = null;
  private statusHandler: ((status: XmppStatusSnapshot) => void) | null = null;
  private reactionHandler: ((event: ReactionEvent) => void) | null = null;
  private displayedHandler: ((event: DisplayedEvent) => void) | null = null;
  private chatStateHandler: ((event: ChatStateEvent) => void) | null = null;
  private hatsHandler: ((hats: RoomHats) => void) | null = null;
  private slowModeHandler: ((seconds: number) => void) | null = null;
  private activityHandler: ((event: RoomActivityEvent) => void) | null = null;
  private roomAvatarHandler: ((roomJid: string, hash: string) => void) | null = null;
  private roomDisconnectHandler: (() => void) | null = null;
  private xmpp: Agent | null = null;
  private connectPromise: Promise<void> | null = null;
  private connected = false;
  private currentRoom: string | null = null;
  private roomSwitchPromise: Promise<void> | null = null;
  private roomSwitchTarget: string | null = null;
  private selfPingTimer: ReturnType<typeof setInterval> | null = null;
  private roomHats: RoomHats = {};

  constructor(session: WaddleSession) { this.session = session; }

  setMessageHandler(h: (message: LiveRoomMessage) => void) { this.messageHandler = h; }
  setStatusHandler(h: (status: XmppStatusSnapshot) => void) { this.statusHandler = h; }
  setChatStateHandler(h: (event: ChatStateEvent) => void) { this.chatStateHandler = h; }
  setReactionHandler(h: (event: ReactionEvent) => void) { this.reactionHandler = h; }
  setDisplayedHandler(h: (event: DisplayedEvent) => void) { this.displayedHandler = h; }
  setHatsHandler(h: (hats: RoomHats) => void) { this.hatsHandler = h; }
  setSlowModeHandler(h: (seconds: number) => void) { this.slowModeHandler = h; }
  setActivityHandler(h: (event: RoomActivityEvent) => void) { this.activityHandler = h; }
  setRoomAvatarHandler(h: (roomJid: string, hash: string) => void) { this.roomAvatarHandler = h; }
  setRoomDisconnectHandler(h: () => void) { this.roomDisconnectHandler = h; }

  // -- Connection lifecycle --

  async connect() {
    if (this.xmpp && this.connected) return;
    if (this.connectPromise) return this.connectPromise;
    if (this.xmpp) {
      try { this.xmpp.disconnect(); } catch { /* ignore stale client */ }
      this.xmpp = null;
    }
    this.currentRoom = null;
    this.roomSwitchPromise = null;
    this.roomSwitchTarget = null;
    this.roomHats = {};
    this.hatsHandler?.({});
    const xmpp = createClient({
      jid: this.session.jid,
      // The session_id is a bearer token — use OAUTHBEARER (RFC 7628)
      credentials: { token: this.session.session_id },
      resource: this.resource,
      transports: { websocket: this.session.xmpp_websocket_url, bosh: false },
      autoReconnect: false,
      useStreamManagement: false,
      sendReceipts: false, chatMarkers: false,
    });
    // Only keep OAUTHBEARER; session tokens aren't SCRAM/PLAIN passwords
    keepOnlyOAuthBearer(xmpp);
    registerWaddleExtensions(xmpp);
    this.wireEvents(xmpp);
    this.xmpp = xmpp;

    // Wait for session:started before returning, so callers can safely
    // use the connection. Reject on disconnect/stream errors.
    const promise = new Promise<void>((resolve, reject) => {
      const cleanup = () => { xmpp.off("session:started", onReady); xmpp.off("disconnected", onFail); };
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
      xmpp.on("disconnected", onFail);
    });
    this.connectPromise = promise;
    xmpp.connect();
    return promise;
  }

  async disconnect() {
    if (!this.xmpp) return;
    const currentRoom = this.currentRoom;
    this.roomSwitchPromise = null;
    this.roomSwitchTarget = null;
    if (currentRoom) {
      try { await this.xmpp.leaveRoom(currentRoom, this.session.username); } catch { /* best-effort */ }
    }
    this.stopSelfPing();
    const xmpp = this.xmpp;
    this.xmpp = null;
    this.connectPromise = null;
    this.connected = false;
    this.currentRoom = null;
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
  async sendCorrection(w: string, c: string, body: string, replacesId: string): Promise<string | null> {
    return this.xmpp ? messaging.sendCorrection(this.xmpp, roomBareJidFor(this.session, w, c), body, replacesId) : null;
  }
  async sendGroupMessage(w: string, c: string, body: string): Promise<string | null> {
    await this.connect(); await this.switchRoom(w, c);
    return this.xmpp ? messaging.sendGroupMessage(this.xmpp, roomBareJidFor(this.session, w, c), body) : null;
  }
  async sendCallInvite(w: string, c: string, meetingUrl: string, video: boolean): Promise<string | null> {
    await this.connect(); await this.switchRoom(w, c);
    return this.xmpp ? messaging.sendCallInvite(this.xmpp, roomBareJidFor(this.session, w, c), meetingUrl, video) : null;
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

  // -- Query delegators --

  async queryMam(w: string, c: string, max = 50): Promise<LiveRoomMessage[]> {
    await this.connect(); await this.switchRoom(w, c);
    return this.xmpp ? history.queryMam(this.xmpp, roomBareJidFor(this.session, w, c), max) : [];
  }
  async searchMessages(w: string, c: string, query: string, max = 20) {
    await this.connect();
    return this.xmpp ? history.searchMessages(this.xmpp, roomBareJidFor(this.session, w, c), query, max) : [];
  }
  async discoverWaddles(): Promise<DiscoveredWaddle[]> {
    await this.connect();
    return this.xmpp ? discovery.discoverWaddles(this.xmpp, this.session.jid) : [];
  }
  async discoverChannels(waddleId: string): Promise<DiscoveredChannel[]> {
    await this.connect();
    return this.xmpp ? discovery.discoverChannels(this.xmpp, this.session.jid, waddleId) : [];
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

  private wireEvents(xmpp: Agent) {
    xmpp.on("session:started", () => {
      if (this.xmpp !== xmpp) return;
      this.connected = true;
      this.statusHandler?.({ state: "online", detail: "Live room connection ready" });
    });
    xmpp.on("disconnected", (err) => {
      if (this.xmpp === xmpp) {
        this.connected = false;
        this.connectPromise = null;
        this.xmpp = null;
        this.currentRoom = null;
        this.roomSwitchPromise = null;
        this.roomSwitchTarget = null;
        this.roomHats = {};
        this.hatsHandler?.({});
        this.stopSelfPing();
      }
      const detail = err?.message ?? "Live room connection offline";
      this.statusHandler?.({ state: "offline", detail });
      console.error("XMPP disconnected", err);
    });
    xmpp.on("stream:error", (streamError, err) => {
      const detail = err?.message ?? streamError?.condition ?? "Stream error";
      this.statusHandler?.({ state: "error", detail });
      console.error("XMPP stream error", detail);
    });

    xmpp.on("muc:available", (pres: ReceivedMUCPresence) => {
      try {
        const [room, nick] = (pres.from ?? "").split("/");
        if (room === this.currentRoom && nick) {
          this.roomHats[nick] = parsePresenceHats(ext(pres).hats ?? ext(pres).hat);
          this.hatsHandler?.({ ...this.roomHats });
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
  }
}

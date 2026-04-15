/** BrowserXmppClient — thin orchestrator that delegates to functional modules. */
import { createClient } from "stanza";
import type { Agent } from "stanza";
import type { ReceivedMUCPresence, ReceivedMessage, ReceivedPresence } from "stanza/protocol";
import type MediaSession from "stanza/jingle/MediaSession";
import type { WaddleSession } from "../server-auth";
import type { WaddleHat } from "./extensions/hats";
import type {
  ChatStateEvent, ChatStateType, DiscoveredChannel, DiscoveredWaddle, DisplayedEvent,
  DmChatStateEvent, DmDisplayedEvent, DmReactionEvent, LiveDmMessage, LiveRoomMessage,
  MujiCallEvent, OccupantPresence, PresenceUpdateEvent, ReactionEvent, RoomActivityEvent,
  RoomHats, RoomPresence, XmppStatusSnapshot,
} from "./types";
import { barePeerJid, roomBareJidFor, sfuServiceJidFor } from "./jid";
import { registerWaddleExtensions } from "./extensions";
import { dispatchGroupchat, ext } from "./message-parsing";
import { dispatchChat } from "./dm-parsing";
import * as messaging from "./messaging";
import * as history from "./history";
import * as dmMessaging from "./dm-messaging";
import * as dmHistory from "./dm-history";
import * as discovery from "./discovery";

type StanzaSaslMechanism = { name: string };
type StanzaSaslFactory = {
  disable(mechanism: string): void;
  mechanisms?: StanzaSaslMechanism[];
};

type MujiSession = MediaSession & {
  sid: string;
  peerID: string;
  state: string;
  connectionState: string;
  pc: RTCPeerConnection;
  includesAudio: boolean;
  includesVideo: boolean;
  addTrack(track: MediaStreamTrack, stream: MediaStream): Promise<void>;
  start(opts?: RTCOfferOptions): Promise<void>;
  accept(opts?: RTCAnswerOptions): Promise<void>;
  end(reason?: string, silent?: boolean): void;
};

function requireMujiSessionShape(session: unknown): MujiSession {
  if (typeof session !== "object" || session === null) {
    throw new Error("Stanza returned an invalid media session object");
  }

  const candidate = session as Partial<MujiSession>;
  if (
    typeof candidate.sid !== "string"
    || typeof candidate.start !== "function"
    || typeof candidate.accept !== "function"
    || typeof candidate.addTrack !== "function"
    || typeof candidate.end !== "function"
  ) {
    throw new Error("Stanza media session shape mismatch");
  }

  return session as MujiSession;
}

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
  private mujiCallHandler: ((event: MujiCallEvent) => void) | null = null;
  private presenceHandler: ((presence: RoomPresence) => void) | null = null;
  private lastSeenHandler: ((nick: string, timestamp: number) => void) | null = null;
  private xmpp: Agent | null = null;
  private connectPromise: Promise<void> | null = null;
  private connected = false;
  private currentRoom: string | null = null;
  private roomSwitchPromise: Promise<void> | null = null;
  private roomSwitchTarget: string | null = null;
  private selfPingTimer: ReturnType<typeof setInterval> | null = null;
  private roomHats: RoomHats = {};
  private roomPresence: RoomPresence = {};
  private readonly mujiSessions = new Map<string, MujiSession>();

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
  setMujiCallHandler(h: (event: MujiCallEvent) => void) { this.mujiCallHandler = h; }
  setPresenceHandler(h: (presence: RoomPresence) => void) { this.presenceHandler = h; }
  setLastSeenHandler(h: (nick: string, timestamp: number) => void) { this.lastSeenHandler = h; }

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
    this.mujiSessions.clear();
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
      autoReconnect: false,
      useStreamManagement: false,
      sendReceipts: false, chatMarkers: false,
    });
    // Only keep OAUTHBEARER; session tokens aren't SCRAM/PLAIN passwords
    keepOnlyOAuthBearer(xmpp);
    registerWaddleExtensions(xmpp);
    if (xmpp.jingle) {
      xmpp.jingle.config.iceServers = [
        { urls: 'stun:stun.l.google.com:19302' },
      ];
    }
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
    this.mujiSessions.clear();
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
  async sendCallInvite(
    w: string,
    c: string,
    opts: { sid?: string; jingleJid?: string; externalUri?: string; video: boolean; muji?: boolean },
  ): Promise<string | null> {
    await this.connect(); await this.switchRoom(w, c);
    return this.xmpp ? messaging.sendCallInvite(this.xmpp, roomBareJidFor(this.session, w, c), opts) : null;
  }
  async sendCallReject(w: string, c: string, inviteId: string): Promise<void> {
    await this.connect();
    await this.switchRoom(w, c);
    if (this.xmpp) messaging.sendCallReject(this.xmpp, roomBareJidFor(this.session, w, c), inviteId);
  }
  async sendCallLeft(w: string, c: string, inviteId?: string): Promise<void> {
    await this.connect();
    await this.switchRoom(w, c);
    if (this.xmpp) messaging.sendCallLeft(this.xmpp, roomBareJidFor(this.session, w, c), inviteId);
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

  async sendDmCallInvite(
    peerJid: string,
    opts: { sid?: string; jingleJid?: string; externalUri?: string; video: boolean; muji?: boolean },
  ): Promise<string | null> {
    await this.connect();
    return this.xmpp ? dmMessaging.sendDmCallInvite(this.xmpp, barePeerJid(peerJid), opts) : null;
  }

  async startMujiCall(
    w: string,
    c: string,
    localStream: MediaStream,
    opts: { video: boolean; sid?: string; serviceJid?: string } = { video: true },
  ): Promise<{ sid: string; serviceJid: string } | null> {
    await this.connect();
    await this.switchRoom(w, c);
    if (!this.xmpp?.jingle) return null;

    const serviceJid = opts.serviceJid ?? sfuServiceJidFor(this.session);
    const sid = opts.sid ?? `${w}_${c}_${crypto.randomUUID()}`;

    // Create real Jingle session to the SFU
    const session = requireMujiSessionShape(
      this.xmpp.jingle.createMediaSession(serviceJid, sid, localStream),
    );
    this.mujiSessions.set(session.sid, session);

    await session.start({
      offerToReceiveAudio: true,
      offerToReceiveVideo: opts.video,
    });

    // Broadcast invite to room
    await this.sendCallInvite(w, c, {
      muji: true,
      sid,
      jingleJid: serviceJid,
      externalUri: `xmpp:${serviceJid}?jingle;sid=${sid}`,
      video: opts.video,
    });

    return { sid, serviceJid };
  }

  async joinMujiCall(
    w: string,
    c: string,
    localStream: MediaStream,
    invite: { sid?: string; jingleJid?: string; video?: boolean },
  ): Promise<{ sid: string; serviceJid: string } | null> {
    await this.connect();
    await this.switchRoom(w, c);
    if (!this.xmpp?.jingle) return null;

    const serviceJid = invite.jingleJid ?? sfuServiceJidFor(this.session);
    const sid = `${w}_${c}_${crypto.randomUUID()}`;

    const session = requireMujiSessionShape(
      this.xmpp.jingle.createMediaSession(serviceJid, sid, localStream),
    );
    this.mujiSessions.set(session.sid, session);
    await session.start({
      offerToReceiveAudio: true,
      offerToReceiveVideo: invite.video ?? true,
    });
    return { sid: session.sid, serviceJid };
  }

  endMujiCall(sid?: string) {
    if (sid) {
      const session = this.mujiSessions.get(sid);
      if (!session) return;
      session.end("success");
      this.mujiSessions.delete(sid);
      return;
    }

    for (const session of this.mujiSessions.values()) {
      session.end("success");
    }
    this.mujiSessions.clear();
  }

  async acceptMujiCall(sid: string, localStream: MediaStream) {
    const session = this.mujiSessions.get(sid);
    if (!session || session.state !== "pending") return;
    for (const track of localStream.getTracks()) {
      if (!session.pc.getSenders().some((sender) => sender.track?.id === track.id)) {
        await session.addTrack(track, localStream);
      }
    }
    await session.accept();
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

  private wireMujiEvents(xmpp: Agent) {
    if (!xmpp.jingle) return;

    xmpp.on("jingle:incoming", (session) => {
      const mediaSession = session as MujiSession;
      this.mujiSessions.set(mediaSession.sid, mediaSession);
      this.mujiCallHandler?.({
        type: "incoming",
        sid: mediaSession.sid,
        peerJid: mediaSession.peerID,
        includesAudio: mediaSession.includesAudio,
        includesVideo: mediaSession.includesVideo,
      });
    });

    xmpp.on("jingle:outgoing", (session) => {
      const mediaSession = session as MujiSession;
      this.mujiSessions.set(mediaSession.sid, mediaSession);
      this.mujiCallHandler?.({
        type: "outgoing",
        sid: mediaSession.sid,
        peerJid: mediaSession.peerID,
      });
    });

    xmpp.on("jingle:accepted", (session) => {
      const mediaSession = session as MujiSession;
      this.mujiSessions.set(mediaSession.sid, mediaSession);
      this.mujiCallHandler?.({ type: "accepted", sid: mediaSession.sid });
    });

    xmpp.on("jingle:terminated", (session, reason) => {
      const mediaSession = session as MujiSession;
      this.mujiSessions.delete(mediaSession.sid);
      const event: MujiCallEvent = {
        type: "terminated",
        sid: mediaSession.sid,
      };
      if (reason?.condition) event.reason = reason.condition;
      this.mujiCallHandler?.(event);
    });

    xmpp.jingle.on("peerTrackAdded", (session, track, stream) => {
      const mediaSession = session as MujiSession;
      this.mujiCallHandler?.({
        type: "peer-track-added",
        sid: mediaSession.sid,
        track,
        stream,
      });
    });

    xmpp.jingle.on("peerTrackRemoved", (session, track) => {
      const mediaSession = session as MujiSession;
      this.mujiCallHandler?.({
        type: "peer-track-removed",
        sid: mediaSession.sid,
        track,
      });
    });

    xmpp.jingle.on("connectionState", (session, state) => {
      const mediaSession = session as MujiSession;
      this.mujiCallHandler?.({
        type: "connection-state",
        sid: mediaSession.sid,
        state,
      });
    });
  }

  private wireEvents(xmpp: Agent) {
    this.wireMujiEvents(xmpp);
    xmpp.on("session:started", async () => {
      if (this.xmpp !== xmpp) return;
      this.connected = true;
      this.statusHandler?.({ state: "online", detail: "Connection ready" });
      try {
        await xmpp.enableCarbons();
      } catch (error) {
        console.warn("Failed to enable message carbons", error);
      }
      try {
        const roster = await xmpp.getRoster();
        for (const item of roster.items ?? []) {
          if (item.jid) xmpp.subscribe(item.jid);
        }
      } catch (error) {
        console.warn("Failed to refresh roster presence subscriptions", error);
      }
    });
    xmpp.on("disconnected", (err) => {
      if (this.xmpp === xmpp) {
        this.connected = false;
        this.connectPromise = null;
        this.xmpp = null;
        this.currentRoom = null;
        this.roomSwitchPromise = null;
        this.roomSwitchTarget = null;
        this.mujiSessions.clear();
        this.roomHats = {};
        this.hatsHandler?.({});
        this.roomPresence = {};
        this.presenceHandler?.({});
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

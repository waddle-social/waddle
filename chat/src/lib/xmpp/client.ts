/** BrowserXmppClient — thin orchestrator that delegates to functional modules. */
import { createClient } from "stanza";
import type { Agent } from "stanza";
import type { ReceivedMUCPresence } from "stanza/protocol";
import type { WaddleSession } from "../server-auth";
import type { WaddleHat } from "./extensions/hats";
import type {
  ChatStateEvent, ChatStateType, DiscoveredChannel, DiscoveredWaddle,
  DisplayedEvent, LiveRoomMessage, ReactionEvent, RoomHats, XmppStatusSnapshot,
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

function keepOnlyOAuthBearer(xmpp: Agent) {
  const sasl = xmpp.sasl as unknown as StanzaSaslFactory;
  if (Array.isArray(sasl.mechanisms)) {
    const oauthBearer = sasl.mechanisms.filter(
      (mechanism) => mechanism.name.toUpperCase() === "OAUTHBEARER",
    );
    if (oauthBearer.length === 0) {
      throw new Error("Stanza OAUTHBEARER SASL mechanism is unavailable");
    }
    sasl.mechanisms = oauthBearer;
    return;
  }

  for (const mech of DISABLED_SASL_MECHANISMS) {
    sasl.disable(mech);
  }
}

export class BrowserXmppClient {
  private readonly session: WaddleSession;
  private messageHandler: ((message: LiveRoomMessage) => void) | null = null;
  private statusHandler: ((status: XmppStatusSnapshot) => void) | null = null;
  private reactionHandler: ((event: ReactionEvent) => void) | null = null;
  private displayedHandler: ((event: DisplayedEvent) => void) | null = null;
  private chatStateHandler: ((event: ChatStateEvent) => void) | null = null;
  private hatsHandler: ((hats: RoomHats) => void) | null = null;
  private slowModeHandler: ((seconds: number) => void) | null = null;
  private activityHandler: ((roomJid: string) => void) | null = null;
  private roomAvatarHandler: ((roomJid: string, hash: string) => void) | null = null;
  private roomDisconnectHandler: (() => void) | null = null;
  private xmpp: Agent | null = null;
  private currentRoom: string | null = null;
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
  setActivityHandler(h: (roomJid: string) => void) { this.activityHandler = h; }
  setRoomAvatarHandler(h: (roomJid: string, hash: string) => void) { this.roomAvatarHandler = h; }
  setRoomDisconnectHandler(h: () => void) { this.roomDisconnectHandler = h; }

  // -- Connection lifecycle --

  async connect() {
    if (this.xmpp) return;
    const xmpp = createClient({
      jid: this.session.jid,
      // The session_id is a bearer token — use OAUTHBEARER (RFC 7628)
      credentials: { token: this.session.session_id },
      resource: "web",
      transports: { websocket: this.session.xmpp_websocket_url, bosh: false },
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
    return new Promise<void>((resolve, reject) => {
      const cleanup = () => { xmpp.off("session:started", onReady); xmpp.off("disconnected", onFail); };
      const onReady = () => { cleanup(); resolve(); };
      const onFail = (err?: Error) => { cleanup(); this.xmpp = null; reject(err ?? new Error("XMPP connection failed")); };
      xmpp.on("session:started", onReady);
      xmpp.on("disconnected", onFail);
      xmpp.connect();
    });
  }

  async disconnect() {
    if (!this.xmpp) return;
    if (this.currentRoom) {
      try { await this.xmpp.leaveRoom(this.currentRoom, this.session.username); } catch { /* best-effort */ }
    }
    this.stopSelfPing();
    const xmpp = this.xmpp;
    this.xmpp = null;
    this.currentRoom = null;
    try { xmpp.disconnect(); } catch { /* ignore */ }
  }

  // -- Room management --

  async switchRoom(waddleId: string, channelId: string) {
    await this.connect();
    const nextRoom = roomBareJidFor(this.session, waddleId, channelId);
    if (this.currentRoom === nextRoom) return;
    if (this.currentRoom && this.xmpp) {
      try { await this.xmpp.leaveRoom(this.currentRoom, this.session.username); } catch { /* best-effort */ }
    }
    this.currentRoom = nextRoom;
    this.roomHats = {};
    this.hatsHandler?.({});
    if (this.xmpp) { await this.xmpp.joinRoom(nextRoom, this.session.username); this.startSelfPing(); }
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
    xmpp.on("session:started", () =>
      this.statusHandler?.({ state: "online", detail: "Live room connection ready" }));
    xmpp.on("disconnected", (err) => {
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
      const [room, nick] = (pres.from ?? "").split("/");
      if (room === this.currentRoom && nick) {
        this.roomHats[nick] = ((ext(pres).hats as WaddleHat[] | undefined) ?? [])
          .map((h) => ({ title: h.title ?? "", uri: h.uri ?? "" }))
          .filter((h) => h.title && h.uri);
        this.hatsHandler?.({ ...this.roomHats });
      }
      if (room && !nick && typeof pres.vcardAvatar === "string" && pres.vcardAvatar)
        this.roomAvatarHandler?.(room, pres.vcardAvatar);
    });

    xmpp.on("muc:unavailable", (pres: ReceivedMUCPresence) => {
      const [room, nick] = (pres.from ?? "").split("/");
      if (room === this.currentRoom && nick) {
        delete this.roomHats[nick];
        this.hatsHandler?.({ ...this.roomHats });
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

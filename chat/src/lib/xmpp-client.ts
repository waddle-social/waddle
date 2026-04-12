import { createClient } from "stanza";
import type { Agent } from "stanza";
import type { ReceivedMessage, ReceivedMUCPresence } from "stanza/protocol";
import type { WaddleHat } from "@/types/stanza-extensions";
import type { WaddleSession } from "./server-auth";
import { registerWaddleExtensions } from "./xmpp-extensions";

export interface XmppStatusSnapshot {
  state: string;
  detail: string;
}

export interface LiveRoomMessage {
  id: string;
  roomJid: string;
  nick: string;
  body: string;
  createdAt: string;
  type: "message" | "subject";
  /** XEP-0308: ID of the message this replaces, if any */
  replacesId?: string;
  /** XEP-0424: ID of the message this retracts, if any */
  retractsId?: string;
  /** XEP-0372: Mentioned JIDs/nicks */
  mentions?: string[];
  /** XEP-0446/0447: Shared file info */
  sharedFile?: SharedFileInfo;
  /** XEP-0449: Is this a sticker message? */
  isSticker?: boolean;
  /** XEP-0513: Broadcast mention type (everyone/here) */
  broadcastMention?: "everyone" | "here";
  /** XEP-0482/0483: Call invite or meeting info */
  callInvite?: CallInviteInfo;
}

/** XEP-0446/0447: Shared file metadata */
export interface SharedFileInfo {
  name?: string;
  mediaType?: string;
  size?: number;
  width?: number;
  height?: number;
  desc?: string;
  url: string;
  disposition: "inline" | "attachment";
}

/** XEP-0482 + XEP-0483: Call invite data extracted from a message */
export interface CallInviteInfo {
  sessionId: string;
  audio: boolean;
  video: boolean;
  externalUri?: string;
  /** XEP-0483: Meeting description */
  meetingDesc?: string;
}

/** XEP-0085 chat state types */
export type ChatStateType = "active" | "composing" | "paused" | "inactive" | "gone";

/** XEP-0317: Hat badge for an occupant */
export interface OccupantHat {
  title: string;
  uri: string;
}

/** XEP-0317: Map of nick -> hats for the current room */
export type RoomHats = Record<string, OccupantHat[]>;


export interface DisplayedEvent {
  roomJid: string;
  nick: string;
  messageId: string;
}

export interface ReactionEvent {
  roomJid: string;
  nick: string;
  messageId: string;
  emojis: string[];
}

export interface ChatStateEvent {
  roomJid: string;
  nick: string;
  state: ChatStateType;
}

export interface DiscoveredWaddle {
  id: string;
  name: string;
  isPublic: boolean;
}

export interface DiscoveredChannel {
  id: string;
  name: string;
}

function jidDomain(jid: string) {
  return jid.split("@")[1] ?? "localhost";
}

function websocketHost(websocketUrl: string) {
  return new URL(websocketUrl).hostname;
}

export function roomBareJidFor(
  session: WaddleSession,
  waddleId: string,
  channelId: string,
) {
  return `${waddleId}_${channelId}@muc.${websocketHost(session.xmpp_websocket_url)}`;
}

/** Helper to access custom JXT extension fields on a stanza message. */
function ext(msg: unknown): Record<string, unknown> {
  return msg as Record<string, unknown>;
}

/** Extract LiveRoomMessage fields from a stanza Message (live or MAM). */
function extractMessageExtensions(
  msg: ReceivedMessage,
  base: LiveRoomMessage,
): void {
  // XEP-0308: Message correction
  if (msg.replace) {
    base.replacesId = msg.replace;
  }

  // XEP-0372: Mention references
  const refs = ext(msg).references as
    | Array<{ type?: string; uri?: string }>
    | undefined;
  if (refs && refs.length > 0) {
    const mentionUris = refs
      .filter((r) => r.type === "mention" && r.uri)
      .map((r) => (r.uri as string).replace(/^xmpp:/, ""));
    if (mentionUris.length > 0) {
      base.mentions = mentionUris;
    }
  }

  // XEP-0513: Explicit mentions (broadcast)
  const explicitMentions = ext(msg).explicitMentions as
    | { items?: Array<{ type?: string }> }
    | undefined;
  if (explicitMentions?.items) {
    for (const m of explicitMentions.items) {
      if (m.type === "everyone") {
        base.broadcastMention = "everyone";
        break;
      }
      if (m.type === "here") {
        base.broadcastMention = "here";
        break;
      }
    }
  }

  // XEP-0482/0483: Call invite
  const callPropose = ext(msg).callPropose as
    | { id?: string; audio?: boolean; video?: boolean; externalUri?: string }
    | undefined;
  if (callPropose) {
    const sessionId = callPropose.id ?? crypto.randomUUID();
    const hasVideo = callPropose.video ?? false;
    const invite: CallInviteInfo = {
      sessionId,
      audio: callPropose.audio ?? !hasVideo,
      video: hasVideo,
    };
    const meeting = ext(msg).meeting as
      | { url?: string; desc?: string }
      | undefined;
    const resolvedUri = callPropose.externalUri ?? meeting?.url;
    if (resolvedUri) invite.externalUri = resolvedUri;
    if (meeting?.desc) invite.meetingDesc = meeting.desc;
    base.callInvite = invite;
  }

  // XEP-0446/0447: File sharing
  const fs = ext(msg).fileSharing as
    | {
        disposition?: string;
        name?: string;
        mediaType?: string;
        size?: string;
        width?: string;
        height?: string;
        desc?: string;
        url?: string;
      }
    | undefined;
  if (fs?.url) {
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
    base.sharedFile = info;
  }

  // XEP-0449: Sticker
  if (ext(msg).sticker) {
    base.isSticker = true;
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

  constructor(session: WaddleSession) {
    this.session = session;
  }

  setMessageHandler(handler: (message: LiveRoomMessage) => void) {
    this.messageHandler = handler;
  }

  setStatusHandler(handler: (status: XmppStatusSnapshot) => void) {
    this.statusHandler = handler;
  }

  setChatStateHandler(handler: (event: ChatStateEvent) => void) {
    this.chatStateHandler = handler;
  }

  setReactionHandler(handler: (event: ReactionEvent) => void) {
    this.reactionHandler = handler;
  }

  setDisplayedHandler(handler: (event: DisplayedEvent) => void) {
    this.displayedHandler = handler;
  }

  setHatsHandler(handler: (hats: RoomHats) => void) {
    this.hatsHandler = handler;
  }

  setSlowModeHandler(handler: (seconds: number) => void) {
    this.slowModeHandler = handler;
  }

  /** XEP-0502: Handler for activity in rooms other than the current one. */
  setActivityHandler(handler: (roomJid: string) => void) {
    this.activityHandler = handler;
  }

  setRoomAvatarHandler(handler: (roomJid: string, hash: string) => void) {
    this.roomAvatarHandler = handler;
  }

  setRoomDisconnectHandler(handler: () => void) {
    this.roomDisconnectHandler = handler;
  }

  private startSelfPing() {
    this.stopSelfPing();
    this.selfPingTimer = setInterval(() => {
      void this.doSelfPing();
    }, 60_000);
  }

  private stopSelfPing() {
    if (this.selfPingTimer) {
      clearInterval(this.selfPingTimer);
      this.selfPingTimer = null;
    }
  }

  private async doSelfPing() {
    if (!this.xmpp || !this.currentRoom) return;

    const occupantJid = `${this.currentRoom}/${this.session.username}`;
    try {
      await this.xmpp.sendIQ({ type: "get", to: occupantJid, ping: true });
    } catch {
      // Error response or timeout → disconnected from room
      this.roomDisconnectHandler?.();
    }
  }

  async connect() {
    if (this.xmpp) {
      return;
    }

    const xmpp = createClient({
      jid: this.session.jid,
      password: this.session.session_id,
      resource: "web",
      transports: {
        websocket: this.session.xmpp_websocket_url,
        bosh: false,
      },
      // Disable auto-sending of receipts and markers — we handle them manually
      sendReceipts: false,
      chatMarkers: false,
    });

    // Register custom XEP protocol definitions
    registerWaddleExtensions(xmpp);

    // -- Connection lifecycle events --

    xmpp.on("session:started", () => {
      this.statusHandler?.({
        state: "online",
        detail: "Live room connection ready",
      });
    });

    xmpp.on("disconnected", () => {
      this.statusHandler?.({
        state: "offline",
        detail: "Live room connection offline",
      });
    });

    // -- MUC presence events (XEP-0045, XEP-0317, XEP-0486) --

    xmpp.on("muc:available", (pres: ReceivedMUCPresence) => {
      const from = pres.from ?? "";
      const [presenceRoom, presenceNick] = from.split("/");
      if (presenceRoom && presenceRoom === this.currentRoom && presenceNick) {
        // XEP-0317: Parse hats from presence
        const hats = ext(pres).hats as WaddleHat[] | undefined;
        if (hats && hats.length > 0) {
          this.roomHats[presenceNick] = hats
            .map((h) => ({ title: h.title ?? "", uri: h.uri ?? "" }))
            .filter((h) => h.title && h.uri);
        } else {
          this.roomHats[presenceNick] = [];
        }
        this.hatsHandler?.({ ...this.roomHats });
      }

      // XEP-0486: Extract room avatar hash from vCard update in presence
      if (presenceRoom && !presenceNick) {
        const vcardAvatar = pres.vcardAvatar;
        if (typeof vcardAvatar === "string" && vcardAvatar) {
          this.roomAvatarHandler?.(presenceRoom, vcardAvatar);
        }
      }
    });

    xmpp.on("muc:unavailable", (pres: ReceivedMUCPresence) => {
      const from = pres.from ?? "";
      const [presenceRoom, presenceNick] = from.split("/");
      if (presenceRoom && presenceRoom === this.currentRoom && presenceNick) {
        delete this.roomHats[presenceNick];
        this.hatsHandler?.({ ...this.roomHats });
      }
    });

    // -- Message error events (XEP-0500: Slow mode) --

    xmpp.on("message:error", (msg) => {
      const error = msg.error;
      if (error?.condition === "policy-violation") {
        const text = error.text ?? "";
        const waitMatch = text.match(/wait\s+(\d+)/i);
        const seconds = waitMatch ? parseInt(waitMatch[1]!, 10) : 0;
        if (seconds > 0) {
          this.slowModeHandler?.(seconds);
        }
      }
    });

    // -- Groupchat messages --

    xmpp.on("groupchat", (msg: ReceivedMessage) => {
      const from = msg.from ?? "";
      const [roomJid, nick = "unknown"] = from.split("/");
      if (!roomJid) return;

      // XEP-0502: If message is for a different room, emit activity indicator
      if (roomJid !== this.currentRoom) {
        if (msg.body) {
          this.activityHandler?.(roomJid);
        }
        return;
      }

      // XEP-0085: Chat state notifications
      if (nick !== this.session.username && msg.chatState) {
        this.chatStateHandler?.({ roomJid, nick, state: msg.chatState as ChatStateType });
      }

      // XEP-0425: Message moderation
      const applyTo = ext(msg).applyTo as
        | { id?: string; moderated?: { retract?: boolean } }
        | undefined;
      if (applyTo?.id && applyTo.moderated) {
        const retractMsg: LiveRoomMessage = {
          id: msg.id ?? crypto.randomUUID(),
          roomJid,
          nick,
          body: "",
          createdAt: new Date().toISOString(),
          type: "message",
          retractsId: applyTo.id,
        };
        this.messageHandler?.(retractMsg);
        return;
      }

      // XEP-0424: Message retraction
      const retract = ext(msg).retract as
        | { id?: string }
        | undefined;
      if (retract?.id) {
        const retractMsg: LiveRoomMessage = {
          id: msg.id ?? crypto.randomUUID(),
          roomJid,
          nick,
          body: "",
          createdAt: new Date().toISOString(),
          type: "message",
          retractsId: retract.id,
        };
        this.messageHandler?.(retractMsg);
        return;
      }

      // XEP-0333: Displayed markers
      if (msg.marker?.type === "displayed" && msg.marker.id && nick !== this.session.username) {
        this.displayedHandler?.({ roomJid, nick, messageId: msg.marker.id });
        return;
      }

      // XEP-0444: Reactions
      const reactions = ext(msg).reactions as
        | { id?: string; items?: string[] }
        | undefined;
      if (reactions?.id) {
        const emojis = (reactions.items ?? []).filter((t) => t.length > 0);
        this.reactionHandler?.({ roomJid, nick, messageId: reactions.id, emojis });
        return;
      }

      const body = msg.body;
      const subject = msg.subject;
      if (!body && !subject) {
        return;
      }

      const liveMsg: LiveRoomMessage = {
        id: msg.id ?? crypto.randomUUID(),
        roomJid,
        nick,
        body: body ?? subject ?? "",
        createdAt: new Date().toISOString(),
        type: body ? "message" : "subject",
      };

      extractMessageExtensions(msg, liveMsg);
      this.messageHandler?.(liveMsg);
    });

    this.xmpp = xmpp;

    try {
      xmpp.connect();
    } catch (error) {
      this.xmpp = null;
      throw error;
    }
  }

  /**
   * XEP-0313: Query the Message Archive (MAM) for a room.
   */
  async queryMam(
    waddleId: string,
    channelId: string,
    max = 50,
  ): Promise<LiveRoomMessage[]> {
    await this.connect();
    await this.switchRoom(waddleId, channelId);
    if (!this.xmpp) return [];

    const roomJid = roomBareJidFor(this.session, waddleId, channelId);

    try {
      const result = await this.xmpp.searchHistory(roomJid, {
        paging: { max },
      });

      const collected: LiveRoomMessage[] = [];
      if (result.results) {
        for (const mamResult of result.results) {
          const innerMsg = mamResult.item?.message;
          if (!innerMsg) continue;

          const from = innerMsg.from ?? "";
          const nick = from.split("/")[1] ?? "unknown";
          const body = innerMsg.body;

          if (body) {
            const archiveId = mamResult.id ?? crypto.randomUUID();
            const timestamp = mamResult.item.delay?.timestamp
              ? mamResult.item.delay.timestamp.toISOString()
              : new Date().toISOString();

            const msg: LiveRoomMessage = {
              id: archiveId,
              roomJid,
              nick,
              body,
              createdAt: timestamp,
              type: "message",
            };

            // Parse reactions from archived messages (XEP-0444)
            const reactions = ext(innerMsg).reactions as
              | { id?: string; items?: string[] }
              | undefined;
            if (reactions?.id) {
              msg.body = "";
              msg.type = "subject"; // marker type — not displayed
              (msg as LiveRoomMessage & { _reactionTarget?: string; _reactionEmojis?: string[] })._reactionTarget = reactions.id;
              (msg as LiveRoomMessage & { _reactionEmojis?: string[] })._reactionEmojis =
                (reactions.items ?? []).filter((t) => t.length > 0);
            }

            extractMessageExtensions(innerMsg as ReceivedMessage, msg);
            collected.push(msg);
          }
        }
      }
      return collected;
    } catch {
      return [];
    }
  }

  /** XEP-0431: Search messages in MAM by full-text query. */
  async searchMessages(
    waddleId: string,
    channelId: string,
    query: string,
    max = 20,
  ): Promise<{ id: string; nick: string; body: string; createdAt: string }[]> {
    await this.connect();
    if (!this.xmpp || !query.trim()) return [];

    const roomJid = roomBareJidFor(this.session, waddleId, channelId);

    try {
      const result = await this.xmpp.searchHistory(roomJid, {
        paging: { max },
        form: {
          type: "submit",
          fields: [
            { name: "FORM_TYPE", type: "hidden", value: "urn:xmpp:mam:2" },
            { name: "fulltext", value: query.trim() },
          ],
        },
      });

      const results: { id: string; nick: string; body: string; createdAt: string }[] = [];
      if (result.results) {
        for (const mamResult of result.results) {
          const innerMsg = mamResult.item?.message;
          if (!innerMsg) continue;

          const from = innerMsg.from ?? "";
          const nick = from.split("/")[1] ?? "unknown";
          const body = innerMsg.body;
          if (body) {
            results.push({
              id: mamResult.id ?? crypto.randomUUID(),
              nick,
              body,
              createdAt: mamResult.item.delay?.timestamp
                ? mamResult.item.delay.timestamp.toISOString()
                : new Date().toISOString(),
            });
          }
        }
      }
      return results;
    } catch {
      return [];
    }
  }

  private parseAccessModel(infoResult: { extensions?: Array<{ fields?: Array<{ name?: string; value?: unknown }> }> }): "open" | "whitelist" | null {
    if (!infoResult.extensions) return null;

    for (const form of infoResult.extensions) {
      if (!form.fields) continue;
      const accessModelField = form.fields.find(
        (field) => field.name === "pubsub#access_model",
      );
      if (!accessModelField) continue;
      const rawValue = accessModelField.value;
      const value = (typeof rawValue === "string" ? rawValue : String(rawValue ?? "")).trim().toLowerCase();
      if (value === "open" || value === "whitelist") {
        return value;
      }
    }

    return null;
  }

  async discoverWaddles(): Promise<DiscoveredWaddle[]> {
    const domain = jidDomain(this.session.jid);
    const spacesDomain = `spaces.${domain}`;

    const response = await this.xmpp!.getDiscoItems(spacesDomain, "");

    const items = response.items ?? [];
    const discovered = items
      .map((item) => ({
        id: item.node ?? "",
        name: item.name ?? item.node ?? "",
      }))
      .filter((w) => w.id);

    const withVisibility = await Promise.all(
      discovered.map(async (waddle) => {
        try {
          const infoResponse = await this.xmpp!.getDiscoInfo(spacesDomain, waddle.id);
          const accessModel = this.parseAccessModel(infoResponse);
          return {
            ...waddle,
            isPublic: accessModel !== "whitelist",
          };
        } catch {
          return {
            ...waddle,
            isPublic: true,
          };
        }
      }),
    );

    return withVisibility;
  }

  async discoverChannels(waddleId: string): Promise<DiscoveredChannel[]> {
    const domain = jidDomain(this.session.jid);
    const spacesDomain = `spaces.${domain}`;

    const response = await this.xmpp!.getDiscoItems(spacesDomain, waddleId);

    const items = response.items ?? [];
    const prefix = `${waddleId}_`;
    return items
      .map((item) => {
        const jid = item.jid ?? "";
        const localPart = jid.split("@")[0] ?? "";
        const channelId = localPart.startsWith(prefix)
          ? localPart.slice(prefix.length)
          : localPart;
        return {
          id: channelId,
          name: item.name ?? channelId,
        };
      })
      .filter((c) => c.id);
  }

  async switchRoom(waddleId: string, channelId: string) {
    await this.connect();

    const nextRoom = roomBareJidFor(this.session, waddleId, channelId);
    if (this.currentRoom === nextRoom) {
      return;
    }

    if (this.currentRoom && this.xmpp) {
      try {
        await this.xmpp.leaveRoom(this.currentRoom, this.session.username);
      } catch {
        // Best-effort leave
      }
    }

    this.currentRoom = nextRoom;
    this.roomHats = {};
    this.hatsHandler?.({});

    if (this.xmpp) {
      await this.xmpp.joinRoom(nextRoom, this.session.username);
      // XEP-0410: Start periodic self-ping to detect disconnection
      this.startSelfPing();
    }
  }

  async sendChatState(waddleId: string, channelId: string, state: ChatStateType) {
    if (!this.xmpp) return;

    this.xmpp.sendMessage({
      to: roomBareJidFor(this.session, waddleId, channelId),
      type: "groupchat",
      chatState: state,
    });
  }

  async sendDisplayed(waddleId: string, channelId: string, messageId: string) {
    if (!this.xmpp) return;

    this.xmpp.sendMessage({
      to: roomBareJidFor(this.session, waddleId, channelId),
      type: "groupchat",
      marker: { type: "displayed", id: messageId },
    });
  }

  async sendReaction(waddleId: string, channelId: string, messageId: string, emojis: string[]) {
    if (!this.xmpp) return;

    this.xmpp.sendMessage({
      id: crypto.randomUUID(),
      to: roomBareJidFor(this.session, waddleId, channelId),
      type: "groupchat",
      reactions: { id: messageId, items: emojis },
    } as Record<string, unknown>);
  }

  async sendRetraction(waddleId: string, channelId: string, retractsId: string) {
    if (!this.xmpp) return;

    this.xmpp.sendMessage({
      id: crypto.randomUUID(),
      to: roomBareJidFor(this.session, waddleId, channelId),
      type: "groupchat",
      body: "This person attempted to retract a previous message.",
      retract: { id: retractsId },
    } as Record<string, unknown>);
  }

  async sendModeration(waddleId: string, channelId: string, targetId: string, reason?: string) {
    if (!this.xmpp) return;

    this.xmpp.sendMessage({
      id: crypto.randomUUID(),
      to: roomBareJidFor(this.session, waddleId, channelId),
      type: "groupchat",
      applyTo: {
        id: targetId,
        moderated: {
          retract: true,
          ...(reason ? { reason } : {}),
        },
      },
    } as Record<string, unknown>);
  }

  async sendCorrection(waddleId: string, channelId: string, body: string, replacesId: string): Promise<string | null> {
    const text = body.trim();
    if (!text) return null;
    if (!this.xmpp) return null;

    const msgId = crypto.randomUUID();

    this.xmpp.sendMessage({
      id: msgId,
      to: roomBareJidFor(this.session, waddleId, channelId),
      type: "groupchat",
      body: text,
      replace: replacesId,
    });

    return msgId;
  }

  async sendGroupMessage(waddleId: string, channelId: string, body: string): Promise<string | null> {
    const text = body.trim();
    if (!text) {
      return null;
    }

    await this.connect();
    await this.switchRoom(waddleId, channelId);

    if (!this.xmpp) {
      return null;
    }

    const msgId = crypto.randomUUID();

    // XEP-0372: Build reference objects for @mentions in the body
    const references: Array<{ type: string; uri: string; begin: string; end: string }> = [];
    const mentionRe = /(?:^|\s)@(\S+)/g;
    let match: RegExpExecArray | null;
    while ((match = mentionRe.exec(text)) !== null) {
      const nick = match[1]!;
      const begin = match.index + (match[0].length - nick.length - 1);
      const end = begin + nick.length + 1;
      references.push({
        type: "mention",
        begin: String(begin),
        end: String(end),
        uri: `xmpp:${nick}`,
      });
    }

    // XEP-0513: Build explicit mentions for @everyone / @here
    const explicitMentionItems: Array<{ type: string }> = [];
    if (/(?:^|\s)@everyone(?:\s|$)/i.test(text)) {
      explicitMentionItems.push({ type: "everyone" });
    }
    if (/(?:^|\s)@here(?:\s|$)/i.test(text)) {
      explicitMentionItems.push({ type: "here" });
    }

    const msgData: Record<string, unknown> = {
      id: msgId,
      to: roomBareJidFor(this.session, waddleId, channelId),
      type: "groupchat",
      body: text,
      receipt: { type: "request" },
      marker: { type: "markable" },
    };

    if (references.length > 0) {
      msgData.references = references;
    }
    if (explicitMentionItems.length > 0) {
      msgData.explicitMentions = { items: explicitMentionItems };
    }

    this.xmpp.sendMessage(msgData as Parameters<Agent["sendMessage"]>[0]);

    return msgId;
  }

  /** XEP-0482 + XEP-0483: Send a call invite to the current room. */
  async sendCallInvite(
    waddleId: string,
    channelId: string,
    meetingUrl: string,
    video: boolean,
  ): Promise<string | null> {
    await this.connect();
    await this.switchRoom(waddleId, channelId);
    if (!this.xmpp) return null;

    const msgId = crypto.randomUUID();
    const sessionId = crypto.randomUUID();

    const label = video ? "Video call" : "Audio call";

    this.xmpp.sendMessage({
      id: msgId,
      to: roomBareJidFor(this.session, waddleId, channelId),
      type: "groupchat",
      body: `${label}: ${meetingUrl}`,
      callPropose: {
        id: sessionId,
        audio: true,
        video,
        externalUri: meetingUrl,
      },
      meeting: {
        type: "jitsi",
        url: meetingUrl,
        desc: label,
      },
    } as Record<string, unknown>);

    return msgId;
  }

  async disconnect() {
    if (!this.xmpp) {
      return;
    }

    if (this.currentRoom) {
      try {
        await this.xmpp.leaveRoom(this.currentRoom, this.session.username);
      } catch {
        // Best-effort leave
      }
    }

    this.stopSelfPing();
    const xmpp = this.xmpp;
    this.xmpp = null;
    this.currentRoom = null;
    try {
      xmpp.disconnect();
    } catch {
      // ignore
    }
  }
}

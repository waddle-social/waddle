import { client, xml } from "@xmpp/client";
import type { XmppElement } from "@xmpp/xml";
import type { WaddleSession } from "./server-auth";

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

const CHATSTATES_NS = "http://jabber.org/protocol/chatstates";

/** XEP-0184 namespace */
const RECEIPTS_NS = "urn:xmpp:receipts";

/** XEP-0308 namespace */
const MESSAGE_CORRECT_NS = "urn:xmpp:message-correct:0";

/** XEP-0424 namespace */
const MESSAGE_RETRACT_NS = "urn:xmpp:message-retract:1";

/** XEP-0425 namespaces */
const MESSAGE_MODERATE_NS = "urn:xmpp:message-moderate:1";
const FASTEN_NS = "urn:xmpp:fasten:0";

/** XEP-0444 namespace */
const REACTIONS_NS = "urn:xmpp:reactions:0";

/** XEP-0333 namespace */
const CHAT_MARKERS_NS = "urn:xmpp:chat-markers:0";

/** XEP-0372 namespace */
const REFERENCES_NS = "urn:xmpp:reference:0";

/** XEP-0513 namespace */
const EXPLICIT_MENTIONS_NS = "urn:xmpp:emn:0";

/** XEP-0482 namespace */
const CALL_INVITES_NS = "urn:xmpp:call-invites:0";

/** XEP-0483 namespace */
const ONLINE_MEETINGS_NS = "urn:xmpp:http:online-meetings:invite:0";

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

function occupantJidFor(
  session: WaddleSession,
  waddleId: string,
  channelId: string,
) {
  return `${roomBareJidFor(session, waddleId, channelId)}/${session.username}`;
}

/** Parse XEP-0447 <file-sharing> from a stanza/message element. */
function parseFileSharing(el: XmppElement): SharedFileInfo | undefined {
  const fsEl = el.getChild("file-sharing");
  if (!fsEl) return undefined;

  const fileEl = fsEl.getChild("file");
  const sourcesEl = fsEl.getChild("sources");
  const urlData = sourcesEl?.children.find((c: XmppElement) => c.is("url-data"));
  const url = urlData?.attrs?.target as string | undefined;
  if (!url) return undefined;

  const disposition = ((fsEl.attrs?.disposition as string) ?? "inline") === "attachment" ? "attachment" as const : "inline" as const;
  const info: SharedFileInfo = { url, disposition };

  if (fileEl) {
    const name = fileEl.getChildText("name");
    if (name) info.name = name;
    const mediaType = fileEl.getChildText("media-type");
    if (mediaType) info.mediaType = mediaType;
    const size = fileEl.getChildText("size");
    if (size) info.size = parseInt(size, 10);
    const width = fileEl.getChildText("width");
    if (width) info.width = parseInt(width, 10);
    const height = fileEl.getChildText("height");
    if (height) info.height = parseInt(height, 10);
    const desc = fileEl.getChildText("desc");
    if (desc) info.desc = desc;
  }

  return info;
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
  private xmpp: ReturnType<typeof client> | null = null;
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
      await this.sendIq(xml("ping", "urn:xmpp:ping"), occupantJid);
    } catch {
      // Error response or timeout → disconnected from room
      this.roomDisconnectHandler?.();
    }
  }

  async connect() {
    if (this.xmpp) {
      return;
    }

    const xmpp = client({
      service: this.session.xmpp_websocket_url,
      domain: jidDomain(this.session.jid),
      resource: "web",
      username: this.session.jid,
      password: this.session.session_id,
    });

    xmpp.on("status", (status: string) => {
      this.statusHandler?.({
        state: status,
        detail:
          status === "online"
            ? "Live room connection ready"
            : status === "offline"
              ? "Live room connection offline"
              : "Live room connection updating",
      });
    });

    xmpp.on("error", (error: Error) => {
      this.statusHandler?.({
        state: "error",
        detail: error.message,
      });
      console.error(error);
    });

    xmpp.on("stanza", (stanza: XmppElement) => {
      // XEP-0317: Parse hats from presence stanzas
      if (stanza.is("presence")) {
        const from = stanza.attrs.from ?? "";
        const [presenceRoom, presenceNick] = from.split("/");
        if (presenceRoom && presenceRoom === this.currentRoom && presenceNick) {
          const isUnavailable = stanza.attrs.type === "unavailable";
          if (isUnavailable) {
            delete this.roomHats[presenceNick];
          } else {
            const hatsEl = stanza.getChild("hats");
            if (hatsEl) {
              const hats: OccupantHat[] = hatsEl.children
                .filter((c: XmppElement) => c.is("hat"))
                .map((c: XmppElement) => ({
                  title: (c.attrs?.title as string) ?? "",
                  uri: (c.attrs?.uri as string) ?? "",
                }))
                .filter((h: OccupantHat) => h.title && h.uri);
              this.roomHats[presenceNick] = hats;
            } else {
              this.roomHats[presenceNick] = [];
            }
          }
          this.hatsHandler?.({ ...this.roomHats });
        }

        // XEP-0486: Extract room avatar hash from vCard update in presence
        if (presenceRoom && !presenceNick) {
          // Bare room JID presence = room's own presence (avatar update)
          const vcardUpdate = stanza.getChild("x");
          if (vcardUpdate) {
            const photoEl = vcardUpdate.getChild("photo");
            if (photoEl) {
              const hash = photoEl.text();
              if (hash) {
                this.roomAvatarHandler?.(presenceRoom, hash);
              }
            }
          }
        }
        return;
      }

      if (!stanza.is("message")) {
        return;
      }

      // XEP-0500: Handle slow mode error responses
      if (stanza.attrs.type === "error") {
        const errorEl = stanza.getChild("error");
        if (errorEl) {
          const policyViolation = errorEl.getChild("policy-violation");
          if (policyViolation) {
            const text = errorEl.getChildText("text") ?? "";
            const waitMatch = text.match(/wait\s+(\d+)/i);
            const seconds = waitMatch ? parseInt(waitMatch[1]!, 10) : 0;
            if (seconds > 0) {
              this.slowModeHandler?.(seconds);
            }
          }
        }
        return;
      }

      const from = stanza.attrs.from ?? "";
      const [roomJid, nick = "unknown"] = from.split("/");
      if (!roomJid) return;

      // XEP-0502: If message is for a different room, emit activity indicator
      if (roomJid !== this.currentRoom) {
        if (stanza.getChildText("body")) {
          this.activityHandler?.(roomJid);
        }
        return;
      }

      // XEP-0085: Check for chat state notifications
      if (nick !== this.session.username) {
        const chatStateNames: ChatStateType[] = ["active", "composing", "paused", "inactive", "gone"];
        for (const name of chatStateNames) {
          if (stanza.getChild(name)) {
            this.chatStateHandler?.({ roomJid, nick, state: name });
            break;
          }
        }
      }

      // XEP-0425: Check for moderation result (before retraction check)
      const applyToEl = stanza.getChild("apply-to");
      if (applyToEl) {
        const moderatedEl = applyToEl.children.find(
          (c: XmppElement) => c.is("moderated"),
        );
        if (moderatedEl) {
          const moderatedId = applyToEl.attrs?.id as string | undefined;
          if (moderatedId) {
            const retractMsg: LiveRoomMessage = {
              id: stanza.attrs.id ?? crypto.randomUUID(),
              roomJid,
              nick,
              body: "",
              createdAt: new Date().toISOString(),
              type: "message",
              retractsId: moderatedId,
            };
            this.messageHandler?.(retractMsg);
            return;
          }
        }
      }

      // XEP-0424: Check for message retraction (before body check)
      const retractEl = stanza.getChild("retract");
      const retractsId = retractEl?.attrs?.id as string | undefined;
      if (retractsId) {
        const retractMsg: LiveRoomMessage = {
          id: stanza.attrs.id ?? crypto.randomUUID(),
          roomJid,
          nick,
          body: "",
          createdAt: new Date().toISOString(),
          type: "message",
          retractsId,
        };
        this.messageHandler?.(retractMsg);
        return;
      }

      // XEP-0333: Check for displayed markers (before body check - markers are body-less)
      const displayedEl = stanza.getChild("displayed");
      if (displayedEl && nick !== this.session.username) {
        const markerMsgId = displayedEl.attrs?.id as string | undefined;
        if (markerMsgId) {
          this.displayedHandler?.({ roomJid, nick, messageId: markerMsgId });
          return;
        }
      }

      // XEP-0444: Check for reactions (before body check - reactions are body-less)
      const reactionsEl = stanza.getChild("reactions");
      if (reactionsEl) {
        const reactedId = reactionsEl.attrs?.id as string | undefined;
        if (reactedId) {
          const emojis: string[] = reactionsEl.children
            .filter((c: XmppElement) => c.is("reaction"))
            .map((c: XmppElement) => c.text())
            .filter((t: string) => t.length > 0);
          this.reactionHandler?.({ roomJid, nick, messageId: reactedId, emojis });
          return;
        }
      }

      const body = stanza.getChildText("body");
      const subject = stanza.getChildText("subject");
      if (!body && !subject) {
        return;
      }

      // XEP-0308: Check for message correction
      const replaceEl = stanza.getChild("replace");
      const replacesId = replaceEl?.attrs?.id as string | undefined;

      const liveMsg: LiveRoomMessage = {
        id: stanza.attrs.id ?? crypto.randomUUID(),
        roomJid,
        nick,
        body: body ?? subject ?? "",
        createdAt: new Date().toISOString(),
        type: body ? "message" : "subject",
      };
      if (replacesId) {
        liveMsg.replacesId = replacesId;
      }

      // XEP-0372: Extract mention references
      const mentionUris: string[] = stanza.children
        .filter((c: XmppElement) => c.is("reference") && c.attrs?.type === "mention")
        .map((c: XmppElement) => (c.attrs?.uri as string) ?? "")
        .filter((u: string) => u.length > 0)
        .map((u: string) => u.replace(/^xmpp:/, ""));
      if (mentionUris.length > 0) {
        liveMsg.mentions = mentionUris;
      }

      // XEP-0513: Extract explicit mentions (broadcast)
      const mentionsEl = stanza.getChild("mentions");
      if (mentionsEl) {
        const mentionChildren = mentionsEl.children.filter(
          (c: XmppElement) => c.is("mention"),
        );
        for (const mc of mentionChildren) {
          const mt = mc.attrs?.type as string | undefined;
          if (mt === "everyone") {
            liveMsg.broadcastMention = "everyone";
            break;
          }
          if (mt === "here") {
            liveMsg.broadcastMention = "here";
            break;
          }
        }
      }

      // XEP-0482: Extract call invite (propose)
      const proposeEl = stanza.getChild("propose");
      if (proposeEl) {
        const sessionId = (proposeEl.attrs?.id as string) ?? crypto.randomUUID();
        const hasAudio = proposeEl.children.some((c: XmppElement) => c.is("audio"));
        const hasVideo = proposeEl.children.some((c: XmppElement) => c.is("video"));
        const externalEl = proposeEl.children.find((c: XmppElement) => c.is("external"));
        const externalUri = externalEl?.attrs?.uri as string | undefined;
        // XEP-0483: Check for meeting element
        const meetingEl = stanza.getChild("meeting");
        const invite: CallInviteInfo = {
          sessionId,
          audio: hasAudio || !hasVideo,
          video: hasVideo,
        };
        const resolvedUri = externalUri ?? (meetingEl?.attrs?.url as string | undefined);
        if (resolvedUri) invite.externalUri = resolvedUri;
        const desc = meetingEl?.attrs?.desc as string | undefined;
        if (desc) invite.meetingDesc = desc;
        liveMsg.callInvite = invite;
      }

      // XEP-0447: Parse file sharing
      const sharedFile = parseFileSharing(stanza);
      if (sharedFile) {
        liveMsg.sharedFile = sharedFile;
      }

      // XEP-0449: Detect sticker
      if (stanza.getChild("sticker")) {
        liveMsg.isSticker = true;
      }

      this.messageHandler?.(liveMsg);
    });

    xmpp.on("online", () => {
      this.statusHandler?.({
        state: "online",
        detail: "Live room connection ready",
      });
    });

    this.xmpp = xmpp;

    try {
      await xmpp.start();
    } catch (error) {
      this.xmpp = null;
      throw error;
    }
  }

  private async sendIq(queryEl: XmppElement, to: string): Promise<XmppElement> {
    await this.connect();
    if (!this.xmpp) throw new Error("XMPP not connected");

    const id = crypto.randomUUID();
    const iq = xml("iq", { type: "get", to, id }, queryEl);

    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.xmpp?.off("stanza", handler);
        reject(new Error("IQ timeout"));
      }, 10000);

      const handler = (stanza: XmppElement) => {
        if (!stanza.is("iq") || stanza.attrs.id !== id) return;
        clearTimeout(timer);
        this.xmpp?.off("stanza", handler);
        if (stanza.attrs.type === "error") {
          reject(new Error("IQ error response"));
        } else {
          resolve(stanza);
        }
      };

      this.xmpp!.on("stanza", handler);
      this.xmpp!.send(iq).catch((err: unknown) => {
        clearTimeout(timer);
        this.xmpp?.off("stanza", handler);
        reject(err);
      });
    });
  }

  /**
   * XEP-0313: Query the Message Archive (MAM) for a room.
   *
   * MAM results arrive as individual <message> stanzas with
   * <result><forwarded><delay/><message/></forwarded></result>
   * followed by a <fin> IQ response.
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
    const queryId = crypto.randomUUID();
    const iqId = crypto.randomUUID();

    const collected: LiveRoomMessage[] = [];

    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.xmpp?.off("stanza", handler);
        // Return whatever we collected even on timeout
        resolve(collected);
      }, 15000);

      const handler = (stanza: XmppElement) => {
        // Collect MAM result messages
        if (stanza.is("message")) {
          const resultEl = stanza.getChild("result");
          if (resultEl?.attrs?.queryid === queryId) {
            const forwarded = resultEl.getChild("forwarded");
            if (forwarded) {
              const delayEl = forwarded.getChild("delay");
              const innerMsg = forwarded.getChild("message");
              if (innerMsg) {
                const from = (innerMsg.attrs?.from as string) ?? "";
                const nick = from.split("/")[1] ?? "unknown";
                const body = innerMsg.getChildText("body");

                if (body) {
                  const archiveId = (resultEl.attrs?.id as string) ?? crypto.randomUUID();
                  const timestamp = (delayEl?.attrs?.stamp as string) ?? new Date().toISOString();

                  const msg: LiveRoomMessage = {
                    id: archiveId,
                    roomJid,
                    nick,
                    body,
                    createdAt: timestamp,
                    type: "message",
                  };

                  // Parse reactions from archived messages (XEP-0444)
                  const reactionsEl = innerMsg.getChild("reactions");
                  if (reactionsEl) {
                    // Reactions in MAM are full reaction sets — handle in caller
                    const reactedId = reactionsEl.attrs?.id as string | undefined;
                    if (reactedId) {
                      msg.body = "";
                      msg.type = "subject"; // marker type — not displayed
                      // Store reaction data on the message for caller to process
                      (msg as LiveRoomMessage & { _reactionTarget?: string; _reactionEmojis?: string[] })._reactionTarget = reactedId;
                      (msg as LiveRoomMessage & { _reactionEmojis?: string[] })._reactionEmojis = reactionsEl.children
                        .filter((c: XmppElement) => c.is("reaction"))
                        .map((c: XmppElement) => c.text())
                        .filter((t: string) => t.length > 0);
                    }
                  }

                  // Parse mention references (XEP-0372)
                  const mentionUris: string[] = innerMsg.children
                    .filter((c: XmppElement) => c.is("reference") && c.attrs?.type === "mention")
                    .map((c: XmppElement) => (c.attrs?.uri as string) ?? "")
                    .filter((u: string) => u.length > 0)
                    .map((u: string) => u.replace(/^xmpp:/, ""));
                  if (mentionUris.length > 0) {
                    msg.mentions = mentionUris;
                  }

                  // Parse broadcast mentions (XEP-0513)
                  const mentionsEl = innerMsg.getChild("mentions");
                  if (mentionsEl) {
                    for (const mc of mentionsEl.children.filter((c: XmppElement) => c.is("mention"))) {
                      const mt = mc.attrs?.type as string | undefined;
                      if (mt === "everyone") { msg.broadcastMention = "everyone"; break; }
                      if (mt === "here") { msg.broadcastMention = "here"; break; }
                    }
                  }

                  // Parse call invites (XEP-0482)
                  const proposeEl = innerMsg.getChild("propose");
                  if (proposeEl) {
                    const sessionId = (proposeEl.attrs?.id as string) ?? crypto.randomUUID();
                    const hasVideo = proposeEl.children.some((c: XmppElement) => c.is("video"));
                    const externalEl = proposeEl.children.find((c: XmppElement) => c.is("external"));
                    const meetingEl = innerMsg.getChild("meeting");
                    const invite: CallInviteInfo = {
                      sessionId,
                      audio: true,
                      video: hasVideo,
                    };
                    const uri = (externalEl?.attrs?.uri as string) ?? (meetingEl?.attrs?.url as string);
                    if (uri) invite.externalUri = uri;
                    const desc = meetingEl?.attrs?.desc as string | undefined;
                    if (desc) invite.meetingDesc = desc;
                    msg.callInvite = invite;
                  }

                  // XEP-0447: Parse file sharing from MAM
                  const sharedFile = parseFileSharing(innerMsg);
                  if (sharedFile) {
                    msg.sharedFile = sharedFile;
                  }

                  // XEP-0449: Detect sticker in MAM
                  if (innerMsg.getChild("sticker")) {
                    msg.isSticker = true;
                  }

                  collected.push(msg);
                }
              }
            }
            return;
          }
        }

        // Wait for the <fin> IQ response
        if (stanza.is("iq") && stanza.attrs.id === iqId) {
          clearTimeout(timer);
          this.xmpp?.off("stanza", handler);
          resolve(collected);
          return;
        }
      };

      this.xmpp!.on("stanza", handler);

      // Send MAM query (type="set" per XEP-0313)
      const queryEl = xml(
        "query",
        { xmlns: "urn:xmpp:mam:2", queryid: queryId },
        xml(
          "x",
          { xmlns: "jabber:x:data", type: "submit" },
          xml("field", { var: "FORM_TYPE", type: "hidden" }, xml("value", {}, "urn:xmpp:mam:2")),
        ),
        xml("set", { xmlns: "http://jabber.org/protocol/rsm" }, xml("max", {}, String(max))),
      );

      this.xmpp!
        .send(xml("iq", { type: "set", to: roomJid, id: iqId }, queryEl))
        .catch((err: unknown) => {
          clearTimeout(timer);
          this.xmpp?.off("stanza", handler);
          reject(err);
        });
    });
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
    const queryId = crypto.randomUUID();
    const iqId = crypto.randomUUID();

    const results: { id: string; nick: string; body: string; createdAt: string }[] = [];

    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.xmpp?.off("stanza", handler);
        resolve(results);
      }, 10000);

      const handler = (stanza: XmppElement) => {
        if (stanza.is("message")) {
          const resultEl = stanza.getChild("result");
          if (resultEl?.attrs?.queryid === queryId) {
            const forwarded = resultEl.getChild("forwarded");
            if (forwarded) {
              const delayEl = forwarded.getChild("delay");
              const innerMsg = forwarded.getChild("message");
              if (innerMsg) {
                const from = (innerMsg.attrs?.from as string) ?? "";
                const nick = from.split("/")[1] ?? "unknown";
                const body = innerMsg.getChildText("body");
                if (body) {
                  results.push({
                    id: (resultEl.attrs?.id as string) ?? crypto.randomUUID(),
                    nick,
                    body,
                    createdAt: (delayEl?.attrs?.stamp as string) ?? new Date().toISOString(),
                  });
                }
              }
            }
          }
          return;
        }
        if (stanza.is("iq") && stanza.attrs.id === iqId) {
          clearTimeout(timer);
          this.xmpp?.off("stanza", handler);
          resolve(results);
        }
      };

      this.xmpp!.on("stanza", handler);

      const queryEl = xml(
        "query",
        { xmlns: "urn:xmpp:mam:2", queryid: queryId },
        xml(
          "x",
          { xmlns: "jabber:x:data", type: "submit" },
          xml("field", { var: "FORM_TYPE", type: "hidden" }, xml("value", {}, "urn:xmpp:mam:2")),
          xml("field", { var: "fulltext" }, xml("value", {}, query.trim())),
        ),
        xml("set", { xmlns: "http://jabber.org/protocol/rsm" }, xml("max", {}, String(max))),
      );

      this.xmpp!
        .send(xml("iq", { type: "set", to: roomJid, id: iqId }, queryEl))
        .catch(() => {
          clearTimeout(timer);
          this.xmpp?.off("stanza", handler);
          resolve(results);
        });
    });
  }

  private parseAccessModel(response: XmppElement): "open" | "whitelist" | null {
    const query = response.getChild("query");
    if (!query) return null;

    for (const form of query.children.filter((child: XmppElement) => child.is("x"))) {
      const fields = form.children.filter((child: XmppElement) => child.is("field"));
      const accessModelField = fields.find(
        (field: XmppElement) => field.attrs.var === "pubsub#access_model",
      );
      if (!accessModelField) continue;
      const value = accessModelField.getChildText("value")?.trim().toLowerCase();
      if (value === "open" || value === "whitelist") {
        return value;
      }
    }

    return null;
  }

  async discoverWaddles(): Promise<DiscoveredWaddle[]> {
    const domain = jidDomain(this.session.jid);
    const spacesDomain = `spaces.${domain}`;

    const response = await this.sendIq(
      xml("query", "http://jabber.org/protocol/disco#items"),
      spacesDomain,
    );

    const query = response.getChild("query");
    if (!query) return [];

    const discovered = query.children
      .filter((item: XmppElement) => item.is("item"))
      .map((item: XmppElement) => ({
        id: item.attrs.node ?? "",
        name: item.attrs.name ?? item.attrs.node ?? "",
      }))
      .filter((w: { id: string; name: string }) => w.id);

    const withVisibility = await Promise.all(
      discovered.map(async (waddle: { id: string; name: string }) => {
        try {
          const infoResponse = await this.sendIq(
            xml("query", {
              xmlns: "http://jabber.org/protocol/disco#info",
              node: waddle.id,
            }),
            spacesDomain,
          );
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

    const response = await this.sendIq(
      xml("query", { xmlns: "http://jabber.org/protocol/disco#items", node: waddleId }),
      spacesDomain,
    );

    const query = response.getChild("query");
    if (!query) return [];

    const prefix = `${waddleId}_`;
    return query.children
      .filter((item: XmppElement) => item.is("item"))
      .map((item: XmppElement) => {
        const jid: string = item.attrs.jid ?? "";
        const localPart = jid.split("@")[0] ?? "";
        const channelId = localPart.startsWith(prefix)
          ? localPart.slice(prefix.length)
          : localPart;
        return {
          id: channelId,
          name: item.attrs.name ?? channelId,
        };
      })
      .filter((c: { id: string; name: string }) => c.id);
  }

  async switchRoom(waddleId: string, channelId: string) {
    await this.connect();

    const nextRoom = roomBareJidFor(this.session, waddleId, channelId);
    if (this.currentRoom === nextRoom) {
      return;
    }

    if (this.currentRoom && this.xmpp) {
      await this.xmpp.send(
        xml(
          "presence",
          {
            to: `${this.currentRoom}/${this.session.username}`,
            type: "unavailable",
          },
          xml("x", "http://jabber.org/protocol/muc"),
        ),
      );
    }

    this.currentRoom = nextRoom;
    this.roomHats = {};
    this.hatsHandler?.({});

    if (this.xmpp) {
      await this.xmpp.send(
        xml(
          "presence",
          {
            to: occupantJidFor(this.session, waddleId, channelId),
          },
          xml("x", "http://jabber.org/protocol/muc"),
        ),
      );
      // XEP-0410: Start periodic self-ping to detect disconnection
      this.startSelfPing();
    }
  }

  async sendChatState(waddleId: string, channelId: string, state: ChatStateType) {
    if (!this.xmpp) return;

    await this.xmpp.send(
      xml(
        "message",
        {
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml(state, CHATSTATES_NS),
      ),
    );
  }

  async sendDisplayed(waddleId: string, channelId: string, messageId: string) {
    if (!this.xmpp) return;

    await this.xmpp.send(
      xml(
        "message",
        {
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml("displayed", { xmlns: CHAT_MARKERS_NS, id: messageId }),
      ),
    );
  }

  async sendReaction(waddleId: string, channelId: string, messageId: string, emojis: string[]) {
    if (!this.xmpp) return;

    const reactionChildren = emojis.map((emoji) => xml("reaction", REACTIONS_NS, emoji));

    await this.xmpp.send(
      xml(
        "message",
        {
          id: crypto.randomUUID(),
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml("reactions", { xmlns: REACTIONS_NS, id: messageId }, ...reactionChildren),
      ),
    );
  }

  async sendRetraction(waddleId: string, channelId: string, retractsId: string) {
    if (!this.xmpp) return;

    await this.xmpp.send(
      xml(
        "message",
        {
          id: crypto.randomUUID(),
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml("body", {}, "This person attempted to retract a previous message."),
        xml("retract", { xmlns: MESSAGE_RETRACT_NS, id: retractsId }),
      ),
    );
  }

  async sendModeration(waddleId: string, channelId: string, targetId: string, reason?: string) {
    if (!this.xmpp) return;

    const moderateChildren = [xml("retract", { xmlns: MESSAGE_RETRACT_NS })];
    if (reason) {
      moderateChildren.push(xml("reason", MESSAGE_MODERATE_NS, reason));
    }

    await this.xmpp.send(
      xml(
        "message",
        {
          id: crypto.randomUUID(),
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml(
          "apply-to",
          { xmlns: FASTEN_NS, id: targetId },
          xml("moderate", MESSAGE_MODERATE_NS, ...moderateChildren),
        ),
      ),
    );
  }

  async sendCorrection(waddleId: string, channelId: string, body: string, replacesId: string): Promise<string | null> {
    const text = body.trim();
    if (!text) return null;
    if (!this.xmpp) return null;

    const msgId = crypto.randomUUID();

    await this.xmpp.send(
      xml(
        "message",
        {
          id: msgId,
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml("body", {}, text),
        xml("replace", { xmlns: MESSAGE_CORRECT_NS, id: replacesId }),
      ),
    );

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

    // XEP-0372: Build <reference> elements for @mentions in the body
    // Use \S+ to match any non-whitespace (supports unicode chars like ø, ü, etc.)
    const references: ReturnType<typeof xml>[] = [];
    const mentionRe = /(?:^|\s)@(\S+)/g;
    let match: RegExpExecArray | null;
    while ((match = mentionRe.exec(text)) !== null) {
      const nick = match[1]!;
      const begin = match.index + (match[0].length - nick.length - 1); // position of '@'
      const end = begin + nick.length + 1; // after the username
      references.push(
        xml("reference", {
          xmlns: REFERENCES_NS,
          type: "mention",
          begin: String(begin),
          end: String(end),
          uri: `xmpp:${nick}`,
        }),
      );
    }

    // XEP-0513: Build <mentions> element for @everyone / @here
    const explicitMentionChildren: ReturnType<typeof xml>[] = [];
    if (/(?:^|\s)@everyone(?:\s|$)/i.test(text)) {
      explicitMentionChildren.push(xml("mention", { type: "everyone" }));
    }
    if (/(?:^|\s)@here(?:\s|$)/i.test(text)) {
      explicitMentionChildren.push(xml("mention", { type: "here" }));
    }
    const extras: ReturnType<typeof xml>[] = [...references];
    if (explicitMentionChildren.length > 0) {
      extras.push(xml("mentions", { xmlns: EXPLICIT_MENTIONS_NS }, ...explicitMentionChildren));
    }

    await this.xmpp.send(
      xml(
        "message",
        {
          id: msgId,
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml("body", {}, text),
        xml("request", RECEIPTS_NS),
        xml("markable", CHAT_MARKERS_NS),
        ...extras,
      ),
    );

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
    const mediaChildren = [xml("audio", {})];
    if (video) mediaChildren.push(xml("video", {}));
    mediaChildren.push(xml("external", { uri: meetingUrl }));

    const label = video ? "Video call" : "Audio call";

    await this.xmpp.send(
      xml(
        "message",
        {
          id: msgId,
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml("body", {}, `${label}: ${meetingUrl}`),
        xml("propose", { xmlns: CALL_INVITES_NS, id: sessionId }, ...mediaChildren),
        xml("meeting", {
          xmlns: ONLINE_MEETINGS_NS,
          type: "jitsi",
          url: meetingUrl,
          desc: label,
        }),
      ),
    );

    return msgId;
  }

  async disconnect() {
    if (!this.xmpp) {
      return;
    }

    if (this.currentRoom) {
      await this.xmpp.send(
        xml(
          "presence",
          {
            to: `${this.currentRoom}/${this.session.username}`,
            type: "unavailable",
          },
          xml("x", "http://jabber.org/protocol/muc"),
        ),
      );
    }

    this.stopSelfPing();
    const xmpp = this.xmpp;
    this.xmpp = null;
    this.currentRoom = null;
    await xmpp.stop().catch(() => undefined);
  }
}

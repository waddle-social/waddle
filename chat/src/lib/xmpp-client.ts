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

export class BrowserXmppClient {
  private readonly session: WaddleSession;
  private messageHandler: ((message: LiveRoomMessage) => void) | null = null;
  private statusHandler: ((status: XmppStatusSnapshot) => void) | null = null;
  private reactionHandler: ((event: ReactionEvent) => void) | null = null;
  private displayedHandler: ((event: DisplayedEvent) => void) | null = null;
  private chatStateHandler: ((event: ChatStateEvent) => void) | null = null;
  private roomDisconnectHandler: (() => void) | null = null;
  private xmpp: ReturnType<typeof client> | null = null;
  private currentRoom: string | null = null;
  private selfPingTimer: ReturnType<typeof setInterval> | null = null;

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
      if (!stanza.is("message")) {
        return;
      }

      const from = stanza.attrs.from ?? "";
      const [roomJid, nick = "unknown"] = from.split("/");
      if (!roomJid || roomJid !== this.currentRoom) {
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
    const references: ReturnType<typeof xml>[] = [];
    const mentionRe = /(?:^|\s)@(\w+)/g;
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
        ...references,
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

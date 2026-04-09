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
}

export interface DiscoveredWaddle {
  id: string;
  name: string;
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
  private xmpp: ReturnType<typeof client> | null = null;
  private currentRoom: string | null = null;

  constructor(session: WaddleSession) {
    this.session = session;
  }

  setMessageHandler(handler: (message: LiveRoomMessage) => void) {
    this.messageHandler = handler;
  }

  setStatusHandler(handler: (status: XmppStatusSnapshot) => void) {
    this.statusHandler = handler;
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

      const body = stanza.getChildText("body");
      const subject = stanza.getChildText("subject");
      if (!body && !subject) {
        return;
      }

      this.messageHandler?.({
        id: stanza.attrs.id ?? crypto.randomUUID(),
        roomJid,
        nick,
        body: body ?? subject ?? "",
        createdAt: new Date().toISOString(),
        type: body ? "message" : "subject",
      });
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
        this.xmpp?.removeListener("stanza", handler);
        reject(new Error("IQ timeout"));
      }, 10000);

      const handler = (stanza: XmppElement) => {
        if (!stanza.is("iq") || stanza.attrs.id !== id) return;
        clearTimeout(timer);
        this.xmpp?.removeListener("stanza", handler);
        if (stanza.attrs.type === "error") {
          reject(new Error("IQ error response"));
        } else {
          resolve(stanza);
        }
      };

      this.xmpp!.on("stanza", handler);
      this.xmpp!.send(iq).catch((err: unknown) => {
        clearTimeout(timer);
        this.xmpp?.removeListener("stanza", handler);
        reject(err);
      });
    });
  }

  async discoverWaddles(): Promise<DiscoveredWaddle[]> {
    const domain = jidDomain(this.session.jid);
    const spacesDomain = `spaces.${domain}`;

    const response = await this.sendIq(
      xml("query", "http://jabber.org/protocol/disco#items"),
      spacesDomain,
    );

    const query = response.getChild("query", "http://jabber.org/protocol/disco#items");
    if (!query) return [];

    return query
      .getChildren("item")
      .map((item) => ({
        id: (item.attrs.node as string | undefined) ?? "",
        name: (item.attrs.name as string | undefined) ?? (item.attrs.node as string | undefined) ?? "",
      }))
      .filter((w) => w.id);
  }

  async discoverChannels(waddleId: string): Promise<DiscoveredChannel[]> {
    const domain = jidDomain(this.session.jid);
    const spacesDomain = `spaces.${domain}`;

    const response = await this.sendIq(
      xml("query", { xmlns: "http://jabber.org/protocol/disco#items", node: waddleId }),
      spacesDomain,
    );

    const query = response.getChild("query", "http://jabber.org/protocol/disco#items");
    if (!query) return [];

    const prefix = `${waddleId}_`;
    return query
      .getChildren("item")
      .map((item) => {
        const jid: string = (item.attrs.jid as string | undefined) ?? "";
        const localPart = jid.split("@")[0] ?? "";
        const channelId = localPart.startsWith(prefix)
          ? localPart.slice(prefix.length)
          : localPart;
        return {
          id: channelId,
          name: (item.attrs.name as string | undefined) ?? channelId,
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
    }
  }

  async sendGroupMessage(waddleId: string, channelId: string, body: string) {
    const text = body.trim();
    if (!text) {
      return;
    }

    await this.connect();
    await this.switchRoom(waddleId, channelId);

    if (!this.xmpp) {
      return;
    }

    await this.xmpp.send(
      xml(
        "message",
        {
          id: crypto.randomUUID(),
          to: roomBareJidFor(this.session, waddleId, channelId),
          type: "groupchat",
        },
        xml("body", {}, text),
      ),
    );
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

    const xmpp = this.xmpp;
    this.xmpp = null;
    this.currentRoom = null;
    await xmpp.stop().catch(() => undefined);
  }
}


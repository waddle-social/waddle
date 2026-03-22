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
  private readonly onMessage: (message: LiveRoomMessage) => void;
  private readonly onStatus: (status: XmppStatusSnapshot) => void;
  private xmpp: ReturnType<typeof client> | null = null;
  private currentRoom: string | null = null;

  constructor(
    session: WaddleSession,
    onMessage: (message: LiveRoomMessage) => void,
    onStatus: (status: XmppStatusSnapshot) => void,
  ) {
    this.session = session;
    this.onMessage = onMessage;
    this.onStatus = onStatus;
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
      this.onStatus({
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
      this.onStatus({
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

      this.onMessage({
        id: stanza.attrs.id ?? crypto.randomUUID(),
        roomJid,
        nick,
        body: body ?? subject ?? "",
        createdAt: new Date().toISOString(),
        type: body ? "message" : "subject",
      });
    });

    xmpp.on("online", () => {
      this.onStatus({
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

import { describe, expect, test } from "bun:test";
import { discoverTopology } from "../src/lib/xmpp/discovery";
import { discoInfoXml, discoItemsXml, pubsubItemsXml, withFakeDomParser } from "./helpers/disco-xml";

type Bookmark = { id: string; name?: string; autojoin?: boolean };

/** A home deployment `example.test` with one Space and an optional parent claim per room. */
function topologyClient(options: {
  rooms: Array<{ jid: string; name?: string }>;
  userBookmarks?: Bookmark[];
  parents?: Record<string, string>;
  serverItemsFail?: boolean;
}) {
  const sent: string[] = [];
  const client = {
    async send_raw_iq(xml: string): Promise<string> {
      sent.push(xml);
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          if (options.serverItemsFail) throw new Error("server items unavailable");
          return discoItemsXml([
            { jid: "spaces.example.test", name: "Spaces" },
            { jid: "muc.example.test", name: "Chatrooms" },
          ]);
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoItemsXml([{ name: "Engineering", node: "space-engineering" }]);
        }
        if (xml.includes('to="muc.example.test"')) return discoItemsXml(options.rooms);
        return discoItemsXml([]);
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (xml.includes('to="muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        const room = /to="([^"]+@muc\.example\.test)"/.exec(xml)?.[1];
        if (room) {
          const parent = options.parents?.[room];
          return discoInfoXml({
            identities: [{ category: "conference", type: "text" }],
            features: ["http://jabber.org/protocol/muc"],
            ...(parent ? { fields: { FORM_TYPE: "urn:xmpp:spaces:0", parent } } : {}),
          });
        }
        if (xml.includes('to="spaces.example.test"') && xml.includes(" node=")) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "leaf" }],
            features: ["http://jabber.org/protocol/pubsub", "urn:xmpp:spaces:0"],
            fields: { FORM_TYPE: "http://jabber.org/protocol/pubsub#meta-data", "pubsub#type": "urn:xmpp:spaces:0" },
          });
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service" }],
            features: ["http://jabber.org/protocol/pubsub", "urn:xmpp:spaces:0"],
          });
        }
        if (xml.includes("remote.test")) throw new Error(`no request to a remote room was expected: ${xml}`);
        return discoInfoXml({ identities: [{ category: "server", type: "im" }] });
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
        return pubsubItemsXml(xml.includes('to="spaces.example.test"') ? [] : options.userBookmarks ?? []);
      }
      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
  return { client, sent };
}

describe("rooms on other servers in the room list", () => {
  test("a bookmarked remote room is listed from its bookmark, under its own id", async () => {
    await withFakeDomParser(async () => {
      const { client, sent } = topologyClient({
        rooms: [{ jid: "general@muc.example.test", name: "General" }],
        userBookmarks: [{ id: "general@muc.remote.test", name: "Their general", autojoin: true }],
      });
      const topology = await discoverTopology(client, "alice@example.test");

      const local = topology.rooms.find((room) => room.jid === "general@muc.example.test");
      const remote = topology.rooms.find((room) => room.jid === "general@muc.remote.test");
      expect(local?.id).toBe("general");
      expect(remote?.id).toBe("general@muc.remote.test");
      expect(remote?.name).toBe("Their general");
      expect(remote?.autojoin).toBe(true);
      expect(remote?.isBookmarked).toBe(true);
      // The structure load never waits on the remote server.
      expect(sent.some((xml) => xml.includes('to="general@muc.remote.test"'))).toBe(false);
      expect(topology.roomCatalogComplete).toBe(true);
    });
  });

  test("without a discovered MUC service no bookmarked room is taken for a remote one", async () => {
    await withFakeDomParser(async () => {
      const { client } = topologyClient({
        rooms: [],
        serverItemsFail: true,
        userBookmarks: [{ id: "general@muc.remote.test", name: "Their general", autojoin: true }],
      });
      const topology = await discoverTopology(client, "alice@example.test");
      // The room is asked about like any bookmark-only room (here its host
      // does not answer, so it is not listed) and gets no qualified id.
      expect(topology.rooms.some((room) => room.id === "general@muc.remote.test")).toBe(false);
    });
  });

  test("a parent Space on another service is not taken for the home Space of the same name", async () => {
    await withFakeDomParser(async () => {
      const { client } = topologyClient({
        rooms: [
          { jid: "claims-remote@muc.example.test", name: "Claims remote" },
          { jid: "claims-home@muc.example.test", name: "Claims home" },
        ],
        parents: {
          "claims-remote@muc.example.test": "xmpp:spaces.remote.test?;node=space-engineering",
          "claims-home@muc.example.test": "xmpp:spaces.example.test?;node=space-engineering",
        },
      });
      const topology = await discoverTopology(client, "alice@example.test");

      expect(topology.rooms.find((room) => room.jid === "claims-remote@muc.example.test")?.spaceId).toBeUndefined();
      expect(topology.rooms.find((room) => room.jid === "claims-home@muc.example.test")?.spaceId).toBe("space-engineering");
    });
  });
});

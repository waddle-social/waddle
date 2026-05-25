import { describe, expect, test } from "bun:test";
import { discoverTopology } from "../src/lib/xmpp/discovery";
import {
  discoInfoXml,
  discoItemsXml,
  pubsubItemsXml,
  withFakeDomParser,
} from "./helpers/disco-xml";

describe("discoverTopology XEP-0402 bookmark autojoin", () => {
  test("surfaces explicit autojoin=true bookmarks", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          bookmarks: [
            { id: "general@muc.example.test", jid: "general@muc.example.test", name: "General", autojoin: true },
          ],
          rooms: [{ jid: "general@muc.example.test", name: "General" }],
        }),
        "alice@example.test",
      );

      expect(topology.rooms.find((room) => room.jid === "general@muc.example.test")?.autojoin).toBe(true);
    });
  });

  test("surfaces explicit autojoin=false bookmarks", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          bookmarks: [
            { id: "muted@muc.example.test", jid: "muted@muc.example.test", name: "Muted", autojoin: false },
          ],
          rooms: [{ jid: "muted@muc.example.test", name: "Muted" }],
        }),
        "alice@example.test",
      );

      expect(topology.rooms.find((room) => room.jid === "muted@muc.example.test")?.autojoin).toBe(false);
    });
  });

  test("defaults omitted bookmark autojoin to false per XEP-0402", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          bookmarks: [
            { id: "foreign@muc.example.test", jid: "foreign@muc.example.test", name: "Foreign" },
          ],
          rooms: [{ jid: "foreign@muc.example.test", name: "Foreign" }],
        }),
        "alice@example.test",
      );

      expect(topology.rooms.find((room) => room.jid === "foreign@muc.example.test")?.autojoin).toBe(false);
    });
  });

  test("defaults orphan sidebar rooms to autojoin=true as Waddle membership convention", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          bookmarks: [],
          rooms: [{ jid: "orphan@muc.example.test", name: "Orphan" }],
        }),
        "alice@example.test",
      );

      expect(topology.rooms.find((room) => room.jid === "orphan@muc.example.test")?.autojoin).toBe(true);
    });
  });
});

type Bookmark = {
  id?: string;
  jid?: string;
  name?: string;
  autojoin?: boolean;
};

function topologyClient(options: {
  bookmarks: Bookmark[];
  rooms: Array<{ jid: string; name?: string }>;
}) {
  return {
    async send_raw_iq(xml: string): Promise<string> {
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          return discoItemsXml([
            { jid: "spaces.example.test", name: "Spaces" },
            { jid: "muc.example.test", name: "Chatrooms" },
          ]);
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoItemsXml([{ name: "Engineering", node: "space-engineering" }]);
        }
        if (xml.includes('to="muc.example.test"')) {
          return discoItemsXml(options.rooms);
        }
        return discoItemsXml([]);
      }

      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (xml.includes('to="muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text", name: "Chatrooms" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service", name: "Spaces" }],
            features: ["urn:xmpp:spaces:0"],
          });
        }
        return discoInfoXml({
          identities: [{ category: "server", type: "im", name: "Waddle" }],
          features: [],
        });
      }

      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
        return pubsubItemsXml(options.bookmarks);
      }

      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
}

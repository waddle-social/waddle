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
            { id: "general@muc.example.test", name: "General", autojoin: true },
          ],
          rooms: [{ jid: "general@muc.example.test", name: "General" }],
        }),
        "alice@example.test",
      );

      const room = topology.rooms.find((candidate) => candidate.jid === "general@muc.example.test");
      expect(room?.autojoin).toBe(true);
      expect(room?.isBookmarked).toBe(true);
    });
  });

  test("surfaces explicit autojoin=false bookmarks", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          bookmarks: [
            { id: "muted@muc.example.test", name: "Muted", autojoin: false },
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
            { id: "foreign@muc.example.test", name: "Foreign" },
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

      const room = topology.rooms.find((candidate) => candidate.jid === "orphan@muc.example.test");
      expect(room?.autojoin).toBe(true);
      expect(room?.isBookmarked).toBe(false);
    });
  });

  test("surfaces XEP-0503 bookmarked rooms even when MUC disco is empty", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          spaceBookmarks: [
            { id: "bookmarked@muc.example.test", name: "Bookmarked", autojoin: true },
          ],
          userBookmarks: [],
          rooms: [],
        }),
        "alice@example.test",
      );

      const room = topology.rooms.find((candidate) => candidate.jid === "bookmarked@muc.example.test");
      expect(room?.name).toBe("Bookmarked");
      expect(room?.spaceId).toBe("space-engineering");
      expect(room?.standalone).toBe(false);
      expect(room?.autojoin).toBe(true);
    });
  });

  test("preserves autojoin=false for disco-confirmed bookmark-only rooms", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          bookmarks: [],
          userBookmarks: [
            { id: "quiet@muc.example.test", name: "Quiet", autojoin: false },
          ],
          rooms: [],
        }),
        "alice@example.test",
      );

      const room = topology.rooms.find((candidate) => candidate.jid === "quiet@muc.example.test");
      expect(room?.name).toBe("Quiet");
      expect(room?.spaceId).toBeUndefined();
      expect(room?.autojoin).toBe(false);
    });
  });

  test("user bookmarks without names do not erase space bookmark names", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          spaceBookmarks: [
            { id: "named@muc.example.test", name: "Named from space", autojoin: true },
          ],
          userBookmarks: [
            { id: "named@muc.example.test", autojoin: false },
          ],
          rooms: [],
        }),
        "alice@example.test",
      );

      const room = topology.rooms.find((candidate) => candidate.jid === "named@muc.example.test");
      expect(room?.name).toBe("Named from space");
      expect(room?.autojoin).toBe(false);
    });
  });

  test("drops bookmark-only rooms that do not disco as MUC rooms", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          bookmarks: [],
          userBookmarks: [
            { id: "not-a-room@example.test", name: "Not a room", autojoin: true },
          ],
          rooms: [],
        }),
        "alice@example.test",
      );

      expect(topology.rooms.some((candidate) => candidate.jid === "not-a-room@example.test")).toBe(false);
    });
  });

  test("ignores non-bookmark XEP-0503 space items when MUC disco is empty", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          bookmarks: [
            { id: "https://example.test/spec", payloadXml: '<x xmlns="jabber:x:oob"><url>https://example.test/spec</url></x>' },
          ],
          rooms: [],
        }),
        "alice@example.test",
      );

      expect(topology.rooms).toEqual([]);
    });
  });
});

type Bookmark = {
  id?: string;
  name?: string;
  autojoin?: boolean;
  payloadXml?: string;
};

function topologyClient(options: {
  bookmarks?: Bookmark[];
  spaceBookmarks?: Bookmark[];
  userBookmarks?: Bookmark[];
  rooms: Array<{ jid: string; name?: string }>;
}) {
  const spaceBookmarks = options.spaceBookmarks ?? options.bookmarks ?? [];
  const userBookmarks = options.userBookmarks ?? options.bookmarks ?? [];
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
        if (xml.includes('@muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        if (xml.includes('to="spaces.example.test"') && xml.includes(' node=')) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "leaf", name: "Engineering" }],
            features: ["http://jabber.org/protocol/pubsub", "urn:xmpp:spaces:0"],
            fields: {
              FORM_TYPE: "http://jabber.org/protocol/pubsub#meta-data",
              "pubsub#type": "urn:xmpp:spaces:0",
            },
          });
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service", name: "Spaces" }],
            features: ["http://jabber.org/protocol/pubsub", "urn:xmpp:spaces:0"],
          });
        }
        return discoInfoXml({
          identities: [{ category: "server", type: "im", name: "Waddle" }],
          features: [],
        });
      }

      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
        return pubsubItemsXml(xml.includes('to="spaces.example.test"') ? spaceBookmarks : userBookmarks);
      }

      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
}

import { describe, expect, test } from "bun:test";
import { discoverTopology, HOME_FEDERATION_FEATURE } from "../src/lib/xmpp/discovery";
import { discoInfoXml, discoItemsXml, pubsubItemsXml, withFakeDomParser } from "./helpers/disco-xml";

type Bookmark = { id: string; name?: string; autojoin?: boolean };

/** A home deployment `example.test` with one Space and an optional parent claim per room. */
function topologyClient(options: {
  rooms: Array<{ jid: string; name?: string }>;
  userBookmarks?: Bookmark[];
  /** Items of the home Space `space-engineering`. */
  spaceItems?: Bookmark[];
  parents?: Record<string, string>;
  serverItemsFail?: boolean;
  /** The home server lists the federation feature (default: it does). */
  federation?: boolean;
  /** The home server's own disco#info fails. */
  serverInfoFails?: boolean;
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
        if (xml.includes("remote.test") || xml.includes("evil.test")) {
          throw new Error(`no request to a remote room was expected: ${xml}`);
        }
        if (options.serverInfoFails) throw new Error("server info unavailable");
        return discoInfoXml({
          identities: [{ category: "server", type: "im" }],
          features: options.federation === false ? [] : [HOME_FEDERATION_FEATURE],
        });
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
        return pubsubItemsXml(xml.includes('to="spaces.example.test"') ? options.spaceItems ?? [] : options.userBookmarks ?? []);
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

  test("without a discovered MUC service a bookmark that cannot be placed waits: not asked, listed or joined", async () => {
    await withFakeDomParser(async () => {
      const { client, sent } = topologyClient({
        rooms: [],
        serverItemsFail: true,
        userBookmarks: [{ id: "general@muc.remote.test", name: "Their general", autojoin: true }],
      });
      const topology = await discoverTopology(client, "alice@example.test");
      expect(topology.services?.mucDiscovered).toBe(false);
      // Nothing is sent to the room's server: no disco#info, no join.
      expect(sent.some((xml) => xml.includes("remote.test"))).toBe(false);
      expect(topology.rooms.some((room) => room.jid === "general@muc.remote.test")).toBe(false);
      expect(topology.unavailableRemoteRooms).toEqual([]);
      // Its absence is no evidence the bookmark is gone.
      expect(topology.roomCatalogComplete).toBe(false);
      expect(topology.roomReconciliationAuthority.absentRoomKeysAuthoritative).toBe(false);
    });
  });

  test("without a discovered MUC service a bookmark of a room the home catalog lists is placed at home", async () => {
    await withFakeDomParser(async () => {
      const { client, sent } = topologyClient({
        rooms: [{ jid: "general@muc.example.test", name: "General" }],
        serverItemsFail: true,
        userBookmarks: [
          { id: "general@muc.example.test", name: "General", autojoin: true },
          { id: "lobby@muc.remote.test", name: "Lobby", autojoin: true },
        ],
      });
      const topology = await discoverTopology(client, "alice@example.test");
      expect(topology.rooms.find((room) => room.jid === "general@muc.example.test")).toMatchObject({
        autojoin: true,
        isBookmarked: true,
      });
      expect(topology.rooms.some((room) => room.jid === "lobby@muc.remote.test")).toBe(false);
      expect(sent.some((xml) => xml.includes("remote.test"))).toBe(false);
    });
  });

  test("a bookmarked remote room is not listed or joined while the home server does not federate", async () => {
    await withFakeDomParser(async () => {
      const { client } = topologyClient({
        rooms: [{ jid: "general@muc.example.test", name: "General" }],
        userBookmarks: [{ id: "general@muc.remote.test", name: "Their general", autojoin: true }],
        federation: false,
      });
      const topology = await discoverTopology(client, "alice@example.test");
      expect(topology.federation).toBe(false);
      expect(topology.rooms.some((room) => room.jid === "general@muc.remote.test")).toBe(false);
      expect(topology.unavailableRemoteRooms).toEqual([{ roomJid: "general@muc.remote.test", name: "Their general" }]);
      expect(topology.rooms.find((room) => room.jid === "general@muc.example.test")).toBeDefined();
    });
  });

  test("an unreadable home disco#info keeps the federation answer of the last discovery", async () => {
    await withFakeDomParser(async () => {
      const userBookmarks = [{ id: "general@muc.remote.test", name: "Their general", autojoin: true }];
      const unknown = await discoverTopology(topologyClient({ rooms: [], userBookmarks, serverInfoFails: true }).client, "alice@example.test");
      expect(unknown.federation).toBeNull();
      expect(unknown.unavailableRemoteRooms).toHaveLength(1);

      const remembered = await discoverTopology(
        topologyClient({ rooms: [], userBookmarks, serverInfoFails: true }).client,
        "alice@example.test",
        { lastKnownFederation: true },
      );
      expect(remembered.rooms.some((room) => room.jid === "general@muc.remote.test")).toBe(true);
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

describe("home Spaces listing rooms on other servers", () => {
  test("a Space item naming a remote room is a suggestion only: not listed, not in the Space, never joined", async () => {
    await withFakeDomParser(async () => {
      const { client, sent } = topologyClient({
        rooms: [{ jid: "general@muc.example.test", name: "General" }],
        spaceItems: [
          { id: "general@muc.example.test", name: "General", autojoin: true },
          { id: "lobby@chat.evil.test", name: "Lobby", autojoin: true },
        ],
      });
      const topology = await discoverTopology(client, "alice@example.test");

      expect(topology.rooms.some((room) => room.jid === "lobby@chat.evil.test")).toBe(false);
      expect(topology.rooms.every((room) => room.autojoin === false || !room.jid?.endsWith("evil.test"))).toBe(true);
      expect(topology.remoteRoomSuggestions).toEqual([
        { spaceId: "space-engineering", roomJid: "lobby@chat.evil.test", name: "Lobby" },
      ]);
      // The home room of the same Space keeps its place and autojoin.
      expect(topology.rooms.find((room) => room.jid === "general@muc.example.test")).toMatchObject({
        spaceId: "space-engineering",
        autojoin: true,
      });
      // Nothing is asked of the remote room's server.
      expect(sent.some((xml) => xml.includes("evil.test"))).toBe(false);
    });
  });

  test("the user's own bookmark lists a Space-named remote room on its own, outside the Space", async () => {
    await withFakeDomParser(async () => {
      const { client } = topologyClient({
        rooms: [],
        spaceItems: [{ id: "dev@chat.remote.test", name: "Dev", autojoin: true }],
        userBookmarks: [{ id: "dev@chat.remote.test", name: "Dev", autojoin: false }],
      });
      const topology = await discoverTopology(client, "alice@example.test");
      const remote = topology.rooms.find((room) => room.jid === "dev@chat.remote.test");
      expect(remote).toMatchObject({ autojoin: false, standalone: true, remoteHost: "chat.remote.test" });
      expect(remote?.spaceId).toBeUndefined();
    });
  });

  test("while the home MUC service is unknown a Space item is placed only by the home catalog", async () => {
    await withFakeDomParser(async () => {
      const { client } = topologyClient({
        rooms: [{ jid: "general@muc.example.test", name: "General" }],
        serverItemsFail: true,
        spaceItems: [
          { id: "general@muc.example.test", name: "General", autojoin: true },
          { id: "lobby@chat.evil.test", name: "Lobby", autojoin: true },
        ],
      });
      const topology = await discoverTopology(client, "alice@example.test");
      expect(topology.services?.mucDiscovered).toBe(false);
      expect(topology.rooms.find((room) => room.jid === "general@muc.example.test")?.spaceId).toBe("space-engineering");
      expect(topology.rooms.some((room) => room.jid === "lobby@chat.evil.test")).toBe(false);
      // It cannot be placed yet, so it is neither a suggestion nor authoritatively absent.
      expect(topology.remoteRoomSuggestions).toEqual([]);
      expect(topology.roomCatalogComplete).toBe(false);
      expect(topology.roomReconciliationAuthority.absentRoomKeysAuthoritative).toBe(false);
    });
  });
});

describe("identifying the home MUC service", () => {
  /**
   * A home deployment `example.test` listing a call mixer, the MUC service
   * and, optionally, a component with a `conference/text` identity alone.
   */
  function componentsClient(options: { mucInfoFails: boolean; identityOnly?: boolean }) {
    return {
      async send_raw_iq(xml: string): Promise<string> {
        if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
          if (xml.includes('to="example.test"')) {
            return discoItemsXml([
              { jid: "calls.example.test" },
              ...(options.identityOnly ? [{ jid: "text.example.test" }] : []),
              { jid: "muc.example.test" },
            ]);
          }
          return discoItemsXml([]);
        }
        if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
          if (xml.includes('to="calls.example.test"')) {
            return discoInfoXml({
              identities: [{ category: "conference", type: "audio-video" }],
              features: ["urn:xmpp:jingle:1"],
            });
          }
          if (/to="[^"@]+@muc\.example\.test"/.test(xml)) {
            return discoInfoXml({
              identities: [{ category: "conference", type: "text" }],
              features: ["http://jabber.org/protocol/muc"],
            });
          }
          if (xml.includes('to="text.example.test"')) {
            return discoInfoXml({ identities: [{ category: "conference", type: "text" }] });
          }
          if (xml.includes('to="muc.example.test"')) {
            if (options.mucInfoFails) throw new Error("muc service did not answer");
            return discoInfoXml({
              identities: [{ category: "conference", type: "text" }],
              features: ["http://jabber.org/protocol/muc"],
            });
          }
          return discoInfoXml({ identities: [{ category: "server", type: "im" }], features: [HOME_FEDERATION_FEATURE] });
        }
        if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
          return pubsubItemsXml([{ id: "general@muc.example.test", name: "General", autojoin: true }]);
        }
        throw new Error(`Unexpected IQ: ${xml}`);
      },
    };
  }

  test("the component advertising XEP-0045 is the MUC service, never the call mixer", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(componentsClient({ mucInfoFails: false }), "alice@example.test/web");
      expect(topology.services).toMatchObject({ muc: "muc.example.test", mucDiscovered: true });
      expect(topology.services?.components).toEqual(["calls.example.test", "muc.example.test"]);
    });
  });

  test("when the MUC service does not answer, the mixer is not taken for it and its rooms stay home", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(componentsClient({ mucInfoFails: true }), "alice@example.test/web");
      expect(topology.services?.muc).toBe("muc.example.test");
      expect(topology.services?.mucDiscovered).toBe(false);
      // The bookmark of a room on a listed component is placed at home.
      expect(topology.rooms.some((room) => room.jid === "general@muc.example.test" && !room.remoteHost)).toBe(true);
      expect(topology.unavailableRemoteRooms ?? []).toEqual([]);
    });
  });

  test("a conference/text identity alone gives the service's address but does not identify it", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        componentsClient({ mucInfoFails: true, identityOnly: true }),
        "alice@example.test/web",
      );
      expect(topology.services).toMatchObject({ muc: "text.example.test", mucDiscovered: false });
    });
  });
});

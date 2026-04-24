import { describe, expect, mock, test } from "bun:test";
import type { Agent } from "stanza";
import { discoverTopology } from "../src/lib/xmpp/discovery";

const NS_MUC = "http://jabber.org/protocol/muc";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_CREATE_NODES = `${NS_PUBSUB}#create-nodes`;
const NS_PUBSUB_CONFIG_NODE = `${NS_PUBSUB}#config-node`;
const NS_PUBSUB_METADATA = `${NS_PUBSUB}#meta-data`;
const NS_PUBSUB_RETRIEVE_ITEMS = `${NS_PUBSUB}#retrieve-items`;
const NS_SPACES = "urn:xmpp:spaces:0";

function metadataForm(fields: Array<{ name: string; value: string }>) {
  return { fields };
}

describe("topology discovery", () => {
  test("discovers protocol creation capabilities and room parent metadata", async () => {
    const getDiscoItems = mock(async (jid: string, node?: string) => {
      if (jid === "muc.example.com") {
        return {
          items: [
            { jid: "general@muc.example.com", name: "General" },
            { jid: "announcements@muc.example.com", name: "Announcements" },
          ],
        };
      }

      if (jid === "spaces.example.com" && !node) {
        return {
          items: [{ jid: "spaces.example.com", node: "space-1", name: "Primary Space" }],
        };
      }

      if (jid === "spaces.example.com" && node === "space-1") {
        return {
          items: [{ jid: "general@muc.example.com", name: "General" }],
        };
      }

      if (jid === "example.com" && node === "http://jabber.org/protocol/commands") {
        throw new Error("legacy command discovery should not be called");
      }

      return { items: [] };
    });

    const getDiscoInfo = mock(async (jid: string, node?: string) => {
      if (jid === "muc.example.com") {
        return {
          features: [NS_MUC],
          identities: [{ category: "conference", type: "text" }],
          extensions: [],
        };
      }

      if (jid === "spaces.example.com" && !node) {
        return {
          features: [
            NS_PUBSUB,
            NS_SPACES,
            NS_PUBSUB_CREATE_NODES,
            NS_PUBSUB_CONFIG_NODE,
            NS_PUBSUB_RETRIEVE_ITEMS,
          ],
          identities: [{ category: "pubsub", type: "service" }],
          extensions: [],
        };
      }

      if (jid === "spaces.example.com" && node === "space-1") {
        return {
          features: [NS_PUBSUB],
          identities: [{ category: "pubsub", type: "leaf" }],
          extensions: [
            metadataForm([
              { name: "FORM_TYPE", value: NS_PUBSUB_METADATA },
              { name: "pubsub#type", value: NS_SPACES },
              { name: "pubsub#title", value: "Primary Space" },
              { name: "pubsub#description", value: "Team hub" },
              { name: "pubsub#owner", value: "alice@example.com" },
              { name: "pubsub#creation_date", value: "2026-01-15T10:00:00Z" },
              { name: "pubsub#access_model", value: "open" },
            ]),
          ],
        };
      }

      if (jid === "general@muc.example.com") {
        return {
          features: [],
          identities: [{ category: "conference", type: "text", name: "General" }],
          extensions: [],
        };
      }

      if (jid === "announcements@muc.example.com") {
        return {
          features: [],
          identities: [{ category: "conference", type: "text", name: "Announcements" }],
          extensions: [
            metadataForm([
              { name: "FORM_TYPE", value: "http://jabber.org/protocol/muc#roominfo" },
              { name: "muc#roominfo_pubsub", value: "xmpp:spaces.example.com?;node=space-1" },
            ]),
            metadataForm([
              { name: "FORM_TYPE", value: NS_SPACES },
              { name: "pubsub#title", value: "Primary Space" },
            ]),
          ],
        };
      }

      throw new Error(`unexpected disco#info lookup for ${jid}${node ? `#${node}` : ""}`);
    });

    const topology = await discoverTopology({
      getDiscoItems,
      getDiscoInfo,
    } as unknown as Agent, "alice@example.com/desktop");

    expect(topology.spaces).toEqual([
      { id: "space-1", name: "Primary Space" },
    ]);
    expect(topology.rooms).toEqual([
      {
        id: "general",
        name: "General",
        jid: "general@muc.example.com",
        channelType: "text",
        position: 0,
        spaceId: "space-1",
        standalone: false,
      },
      {
        id: "announcements",
        name: "Announcements",
        jid: "announcements@muc.example.com",
        channelType: "text",
        position: 1,
        standalone: true,
      },
    ]);
    expect(getDiscoItems.mock.calls).not.toContainEqual([
      "example.com",
      "http://jabber.org/protocol/commands",
    ]);
  });

  test("does not fall back to legacy command nodes when protocol support is absent", async () => {
    const getDiscoItems = mock(async (jid: string, node?: string) => {
      if (jid === "example.com" && node === "http://jabber.org/protocol/commands") {
        throw new Error("legacy command fallback should not run");
      }
      return { items: [] };
    });

    const getDiscoInfo = mock(async (jid: string) => {
      if (jid === "muc.example.com") {
        return {
          features: [],
          identities: [],
          extensions: [],
        };
      }

      if (jid === "spaces.example.com") {
        return {
          features: [NS_PUBSUB, NS_SPACES, NS_PUBSUB_CREATE_NODES],
          identities: [{ category: "pubsub", type: "service" }],
          extensions: [],
        };
      }

      return {
        features: [],
        identities: [],
        extensions: [],
      };
    });

    const topology = await discoverTopology({
      getDiscoItems,
      getDiscoInfo,
    } as unknown as Agent, "alice@example.com/desktop");

    expect(topology).toEqual({ spaces: [], rooms: [] });
    expect(getDiscoItems.mock.calls).not.toContainEqual([
      "example.com",
      "http://jabber.org/protocol/commands",
    ]);
  });
});

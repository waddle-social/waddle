import { describe, expect, mock, test } from "bun:test";
import { discoverChannels } from "../src/lib/xmpp/discovery";

function makeAgent() {
  const getDiscoItems = mock(async (_jid: string, node?: string) => ({
    items: node === undefined ? [{ jid: "spaces.example.com", node: "space", name: "Example" }] : [],
  }));
  const getItems = mock(async (_jid: string, node: string) => ({
    items:
      node === "space"
        ? [
            { id: "general@muc.chat.example.com", content: { itemType: "urn:xmpp:bookmarks:1", name: "General" } },
            { id: "roadmap@muc.chat.example.com", content: { itemType: "urn:xmpp:bookmarks:1", name: "Roadmap" } },
          ]
        : [],
  }));
  const getDiscoInfo = mock(async (jid: string) => {
    if (jid === "roadmap@muc.chat.example.com") {
      return {
        features: ["urn:xmpp:forums:0"],
        identities: [{ name: "Roadmap" }],
        extensions: [],
      };
    }
    return {
      features: [],
      identities: [{ name: "General" }],
      extensions: [],
    };
  });

  return {
    getDiscoItems,
    getDiscoInfo,
    getItems,
    agent: {
      getDiscoItems,
      getDiscoInfo,
      getItems,
    } as unknown as Agent,
  };
}

describe("forum channel discovery", () => {
  test("detects forum rooms from disco features and preserves order", async () => {
    const xmpp = makeAgent();
    const channels = await discoverChannels(xmpp.agent, "alice@example.com/desktop");

    expect(channels).toEqual([
      { id: "general", name: "General", channelType: "text", position: 0 },
      { id: "roadmap", name: "Roadmap", channelType: "forum", position: 1 },
    ]);
    expect(xmpp.getDiscoInfo.mock.calls).toEqual([
      ["general@muc.chat.example.com"],
      ["roadmap@muc.chat.example.com"],
    ]);
    expect(xmpp.getDiscoItems.mock.calls).toEqual([
      ["spaces.example.com"],
    ]);
    expect(xmpp.getItems.mock.calls).toEqual([
      ["spaces.example.com", "space"],
    ]);
  });

  test("treats muc#roomconfig_forum as forum capability when feature discovery is absent", async () => {
    const getDiscoItems = mock(async (_jid: string, node?: string) => ({
      items: node === undefined ? [{ jid: "spaces.example.com", node: "space", name: "Example" }] : [],
    }));
    const getItems = mock(async (_jid: string, node: string) => ({
      items:
        node === "space"
          ? [{ id: "ideas@muc.chat.example.com", content: { itemType: "urn:xmpp:bookmarks:1", name: "Ideas" } }]
          : [],
    }));
    const getDiscoInfo = mock(async () => ({
      features: [],
      identities: [{ name: "Ideas" }],
      extensions: [
        {
          fields: [{ name: "muc#roomconfig_forum", value: "true" }],
        },
      ],
    }));
    const xmpp = {
      getDiscoItems,
      getDiscoInfo,
      getItems,
    } as unknown as Agent;

    const channels = await discoverChannels(xmpp, "alice@example.com/desktop");

    expect(channels).toEqual([
      { id: "ideas", name: "Ideas", channelType: "forum", position: 0 },
    ]);
    expect(getDiscoInfo.mock.calls).toEqual([
      ["ideas@muc.chat.example.com"],
    ]);
  });

  test("returns no channels when the server has no discoverable space node", async () => {
    const getDiscoItems = mock(async () => ({ items: [] }));
    const getDiscoInfo = mock(async () => ({
      features: [],
      identities: [],
      extensions: [],
    }));
    const xmpp = {
      getDiscoItems,
      getDiscoInfo,
    } as unknown as Agent;

    const channels = await discoverChannels(xmpp, "alice@example.com/desktop");

    expect(channels).toEqual([]);
    expect(getDiscoInfo).not.toHaveBeenCalled();
  });
});

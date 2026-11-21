import { describe, expect, test } from "bun:test";
import {
  describeCommunityAddress,
  fetchSpaceContent,
  parseDirectoryItems,
  parseDiscoInfo,
  parseSpaceContent,
  parseSpaceSubscription,
  roomDescriptionFromInfo,
  spaceDescriptionFromInfo,
  subscribeToSpace,
  unsubscribeFromSpace,
} from "../src/lib/xmpp/remote-community";
import { discoInfoXml, discoItemsXml, withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

const NS_PUBSUB = "http://jabber.org/protocol/pubsub";

function withXml(run: () => Promise<void>): Promise<void> {
  return withFakeDomParser(() => withFakeXmlDocument(run));
}

function parse(xml: string): Document {
  return new DOMParser().parseFromString(xml, "text/xml");
}

function stanzaError(condition: string, errorType = "cancel"): Error {
  return Object.assign(new Error(condition), { condition, errorType });
}

describe("what a host says an address is", () => {
  test("a room's flags and its parent claim are read from its disco#info", async () => {
    await withFakeDomParser(async () => {
      const info = parseDiscoInfo(parse(discoInfoXml({
        identities: [{ category: "conference", type: "text", name: "Developers" }],
        features: ["http://jabber.org/protocol/muc", "muc_membersonly", "muc_nonanonymous", "muc_moderated"],
        fields: {
          FORM_TYPE: "http://jabber.org/protocol/muc#roominfo",
          "muc#roominfo_description": "Talk about the code",
          "muc#roomconfig_pubsub": "xmpp:spaces.remote.test?;node=eng",
        },
      })));
      expect(roomDescriptionFromInfo("dev@chat.remote.test", info)).toEqual({
        kind: "room",
        roomJid: "dev@chat.remote.test",
        name: "Developers",
        description: "Talk about the code",
        flags: { membersOnly: true, passwordProtected: false, hidden: false, moderated: true, nonAnonymous: true },
        parent: { serviceJid: "spaces.remote.test", node: "eng" },
      });
    });
  });

  test("an account is not a room", async () => {
    await withFakeDomParser(async () => {
      const info = parseDiscoInfo(parse(discoInfoXml({ identities: [{ category: "account", type: "registered" }] })));
      expect(roomDescriptionFromInfo("bob@remote.test", info)).toBeNull();
    });
  });

  test("a Space is recognised by its pubsub#type, without the Spaces feature", async () => {
    await withFakeDomParser(async () => {
      const info = parseDiscoInfo(parse(discoInfoXml({
        identities: [{ category: "pubsub", type: "leaf" }],
        features: ["http://jabber.org/protocol/pubsub"],
        fields: {
          FORM_TYPE: "http://jabber.org/protocol/pubsub#meta-data",
          "pubsub#type": "urn:xmpp:spaces:0",
          "pubsub#title": "Engineering",
        },
      })));
      expect(spaceDescriptionFromInfo("spaces.remote.test", "eng", info)).toEqual({
        kind: "space",
        serviceJid: "spaces.remote.test",
        node: "eng",
        title: "Engineering",
        description: null,
      });
    });
  });

  test("a PubSub node of another type is not a Space", async () => {
    await withFakeDomParser(async () => {
      const info = parseDiscoInfo(parse(discoInfoXml({
        identities: [{ category: "pubsub", type: "leaf" }],
        fields: { FORM_TYPE: "http://jabber.org/protocol/pubsub#meta-data", "pubsub#type": "urn:xmpp:microblog:0" },
      })));
      expect(spaceDescriptionFromInfo("pubsub.remote.test", "blog", info)).toBeNull();
    });
  });

  test("a service listing is read by the kind of service that lists it", async () => {
    await withFakeDomParser(async () => {
      const muc = parseDiscoInfo(parse(discoInfoXml({ identities: [{ category: "conference", type: "text" }] })));
      expect(parseDirectoryItems(parse(discoItemsXml([
        { jid: "Dev@Chat.Remote.test", name: "Developers" },
        { jid: "chat.remote.test", node: "ignored" },
      ])), muc)).toEqual([{ kind: "room", roomJid: "dev@chat.remote.test", name: "Developers" }]);

      const pubsub = parseDiscoInfo(parse(discoInfoXml({ identities: [{ category: "pubsub", type: "service" }] })));
      expect(parseDirectoryItems(parse(discoItemsXml([{ jid: "spaces.remote.test", node: "eng", name: "Engineering" }])), pubsub))
        .toEqual([{ kind: "space", serviceJid: "spaces.remote.test", node: "eng", name: "Engineering" }]);

      const server = parseDiscoInfo(parse(discoInfoXml({ identities: [{ category: "server", type: "im" }] })));
      expect(parseDirectoryItems(parse(discoItemsXml([{ jid: "chat.remote.test", name: "Chatrooms" }])), server))
        .toEqual([{ kind: "service", jid: "chat.remote.test", name: "Chatrooms" }]);
    });
  });

  test("a lookup asks the remote address itself and allows for the router's own timeout", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = {
        async send_raw_iq(xml: string) {
          sent.push(xml);
          return discoInfoXml({ identities: [{ category: "conference", type: "text", name: "Dev" }] });
        },
      };
      const description = await describeCommunityAddress(xmpp, { kind: "room", roomJid: "dev@chat.remote.test" });
      expect(description.kind).toBe("room");
      expect(sent).toHaveLength(1);
      expect(sent[0]).toContain('to="dev@chat.remote.test"');
      expect(sent[0]).toContain('<query xmlns="http://jabber.org/protocol/disco#info">');
    });
  });

  test("a failed lookup rejects with the host's stanza error", async () => {
    await withXml(async () => {
      const xmpp = { async send_raw_iq() { throw stanzaError("remote-server-timeout", "wait"); } };
      await expect(describeCommunityAddress(xmpp, { kind: "space", serviceJid: "spaces.remote.test", node: "eng" }))
        .rejects.toMatchObject({ condition: "remote-server-timeout" });
    });
  });
});

describe("Space content (D4: images are never fetched)", () => {
  const AVATAR_URL = "https://images.remote.test/avatar.jpg";
  const BANNER_URL = "https://images.remote.test/banner.jpg";
  const itemsXml = `<iq type="result"><pubsub xmlns="${NS_PUBSUB}"><items node="eng">`
    + `<item id="Dev@Chat.Remote.test"><conference xmlns="urn:xmpp:bookmarks:1" name="Developers" autojoin="true"/></item>`
    + `<item id="ops@chat.remote.test"><conference xmlns="urn:xmpp:bookmarks:1"/></item>`
    + `<item id="urn:xmpp:spaces:avatar:metadata:0"><metadata xmlns="urn:xmpp:avatar:metadata"><info bytes="420" height="64" width="64" type="image/jpeg" url="${AVATAR_URL}" id="sha1"/></metadata></item>`
    + `<item id="urn:xmpp:spaces:banner:metadata:0"><metadata xmlns="urn:xmpp:avatar:metadata"><info bytes="4242" height="64" width="256" type="image/jpeg" url="${BANNER_URL}" id="sha1b"/></metadata></item>`
    + `<item id="some-hash"><subscription xmlns="urn:xmpp:pubsub:subscription:0" server="pubsub.remote.test" node="news"/></item>`
    + `</items></pubsub></iq>`;

  test("rooms are listed and images are only noted as present", async () => {
    await withFakeDomParser(async () => {
      const content = parseSpaceContent(parse(itemsXml));
      expect(content.rooms).toEqual([
        { roomJid: "dev@chat.remote.test", name: "Developers", suggestedAutojoin: true },
        { roomJid: "ops@chat.remote.test", name: null, suggestedAutojoin: false },
      ]);
      expect(content.hasAvatar).toBe(true);
      expect(content.hasBanner).toBe(true);
      expect(content.otherItems).toBe(1);
    });
  });

  test("no image URL survives parsing, so nothing can fetch one", async () => {
    await withFakeDomParser(async () => {
      const serialized = JSON.stringify(parseSpaceContent(parse(itemsXml)));
      expect(serialized).not.toContain(AVATAR_URL);
      expect(serialized).not.toContain(BANNER_URL);
      expect(serialized).not.toContain("https://");
    });
  });

  test("reading a Space makes one request to its service, and nothing else", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const originalFetch = globalThis.fetch;
      let fetched = 0;
      globalThis.fetch = (async () => {
        fetched += 1;
        throw new Error("no fetch expected");
      }) as unknown as typeof fetch;
      try {
        const xmpp = { async send_raw_iq(xml: string) { sent.push(xml); return itemsXml; } };
        await fetchSpaceContent(xmpp, "spaces.remote.test", "eng");
      } finally {
        globalThis.fetch = originalFetch;
      }
      expect(sent).toHaveLength(1);
      expect(sent[0]).toContain('to="spaces.remote.test"');
      expect(fetched).toBe(0);
    });
  });
});

describe("following a Space", () => {
  test("a subscription result reports its state and optional ID", async () => {
    await withFakeDomParser(async () => {
      expect(parseSpaceSubscription(parse(`<iq type="result"><pubsub xmlns="${NS_PUBSUB}"><subscription node="eng" jid="alice@home.test" subscription="pending" subid="host-token"/></pubsub></iq>`)))
        .toEqual({ state: "pending", id: "host-token" });
      expect(parseSpaceSubscription(parse('<iq type="result"/>'))).toEqual({ state: "subscribed", id: null });
    });
  });

  test("subscribing names the account's bare JID", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = { async send_raw_iq(xml: string) { sent.push(xml); return '<iq type="result"/>'; } };
      expect(await subscribeToSpace(xmpp, "spaces.remote.test", "eng", "alice@home.test")).toEqual({ state: "subscribed", id: null });
      expect(sent[0]).toContain('<subscribe node="eng" jid="alice@home.test">');
    });
  });

  test("not being subscribed, or a node the host no longer knows, leaves nothing to end", async () => {
    await withXml(async () => {
      for (const condition of ["unexpected-request", "item-not-found"]) {
        const xmpp = { async send_raw_iq() { throw stanzaError(condition); } };
        await expect(unsubscribeFromSpace(xmpp, "spaces.remote.test", "eng", "alice@home.test")).resolves.toBeUndefined();
      }
    });
  });

  test("an outage while unsubscribing is reported, not taken for success", async () => {
    await withXml(async () => {
      const xmpp = { async send_raw_iq() { throw stanzaError("remote-server-timeout", "wait"); } };
      await expect(unsubscribeFromSpace(xmpp, "spaces.remote.test", "eng", "alice@home.test"))
        .rejects.toMatchObject({ condition: "remote-server-timeout" });
    });
  });
});

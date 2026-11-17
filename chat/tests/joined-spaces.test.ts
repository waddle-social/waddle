import { describe, expect, test } from "bun:test";
import {
  buildClearLegacyJoinedSpacesIq,
  buildLoadJoinedSpacesIq,
  buildLoadLegacyJoinedSpacesIq,
  buildPutJoinedSpaceIq,
  buildRemoveJoinedSpaceIq,
  joinedSpaceItemId,
  joinedSpaceKey,
  localSpaceUnfollowIntentStore,
  parseJoinedSpaceItems,
  parseJoinedSpaces,
  pepJoinedSpaceStore,
} from "../src/lib/xmpp/joined-spaces";
import { withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

const NODE = "urn:waddle:joined-spaces:0";

function withXml(run: () => Promise<void>): Promise<void> {
  return withFakeDomParser(() => withFakeXmlDocument(run));
}

function legacyXml(spaces: string): string {
  return `<iq type="result"><query xmlns="jabber:iq:private"><spaces xmlns="${NODE}">${spaces}</spaces></query></iq>`;
}

function itemsXml(items: Array<{ id: string; space: string }>): string {
  return `<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="${NODE}">${
    items.map((item) => `<item id="${item.id}"><space xmlns="${NODE}" ${item.space}/></item>`).join("")
  }</items></pubsub></iq>`;
}

function stanzaError(condition: string, errorType = "cancel"): Error {
  return Object.assign(new Error(condition), { condition, errorType });
}

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    values,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
}

describe("joined-space entries", () => {
  test("entries are keyed by service and node together, and each has one canonical item id", () => {
    expect(joinedSpaceKey({ serviceJid: "spaces.a.test", node: "main" }))
      .not.toBe(joinedSpaceKey({ serviceJid: "spaces.b.test", node: "main" }));
    expect(joinedSpaceKey({ serviceJid: "Spaces.A.test.", node: "main" }))
      .toBe(joinedSpaceKey({ serviceJid: "spaces.a.test", node: "main" }));
    expect(joinedSpaceItemId({ serviceJid: "Spaces.A.test.", node: "main" })).toBe("xmpp:spaces.a.test?;node=main");
    expect(joinedSpaceItemId({ serviceJid: "spaces.a.test", node: "a b" })).toBe("xmpp:spaces.a.test?;node=a%20b");
  });

  test("items are read only under the canonical id of the Space they name", async () => {
    await withFakeDomParser(async () => {
      const doc = new DOMParser().parseFromString(itemsXml([
        { id: "xmpp:spaces.a.test?;node=main", space: 'service="spaces.a.test" node="main" name="Main" follow="true" role="owner"' },
        { id: "xmpp:spaces.a.test?;node=other", space: 'service="spaces.b.test" node="main"' },
        { id: "xmpp:spaces.b.test?;node=main", space: 'service="spaces.b.test" node="main"' },
      ]), "text/xml");
      const preferences = parseJoinedSpaceItems(doc);
      expect(preferences).toEqual([
        { serviceJid: "spaces.a.test", node: "main", name: "Main", follow: true },
        { serviceJid: "spaces.b.test", node: "main", follow: false },
      ]);
      // A preference is never a standing: no role or affiliation is read.
      expect(Object.keys(preferences[0]!).sort()).toEqual(["follow", "name", "node", "serviceJid"]);
    });
  });

  test("a document of earlier versions is read without duplicates", async () => {
    await withFakeDomParser(async () => {
      const doc = new DOMParser().parseFromString(legacyXml(
        '<space service="spaces.a.test" node="main" follow="true"/>'
        + '<space service="SPACES.A.test" node="main" name="Duplicate"/>'
        + '<space service="" node="orphan"/>',
      ), "text/xml");
      expect(parseJoinedSpaces(doc)).toEqual([{ serviceJid: "spaces.a.test", node: "main", follow: true }]);
    });
  });
});

describe("joined-space requests", () => {
  test("one Space is published as one private PEP item under its canonical id", async () => {
    await withXml(async () => {
      const xml = buildPutJoinedSpaceIq("p1", { serviceJid: "Spaces.A.test.", node: "main", name: "Main", follow: true });
      expect(xml).toContain(`<publish node="${NODE}">`);
      expect(xml).toContain('<item id="xmpp:spaces.a.test?;node=main">');
      expect(xml).toContain(`<space xmlns="${NODE}" service="spaces.a.test" node="main" name="Main" follow="true">`);
      // XEP-0223 publish-options: persistent and owner-only.
      expect(xml).toContain('<field var="pubsub#persist_items"><value>true</value></field>');
      expect(xml).toContain('<field var="pubsub#access_model"><value>whitelist</value></field>');
      // Addressed to the account itself: no `to`, so the home server answers.
      expect(xml).not.toContain(" to=");
    });
  });

  test("removing a Space retracts its one item and notifies the user's other clients", async () => {
    await withXml(async () => {
      const xml = buildRemoveJoinedSpaceIq("r1", { serviceJid: "spaces.a.test", node: "main" });
      expect(xml).toContain(`<retract node="${NODE}" notify="true">`);
      expect(xml).toContain('<item id="xmpp:spaces.a.test?;node=main">');
    });
  });

  test("the list is read from the PEP node and the old document from private XML storage", async () => {
    await withXml(async () => {
      expect(buildLoadJoinedSpacesIq("l1")).toContain(`<items node="${NODE}">`);
      expect(buildLoadLegacyJoinedSpacesIq("l2")).toContain(`<query xmlns="jabber:iq:private"><spaces xmlns="${NODE}">`);
      const clear = buildClearLegacyJoinedSpacesIq("c1");
      expect(clear).toContain('type="set"');
      expect(clear).toContain(`<spaces xmlns="${NODE}"></spaces>`);
    });
  });
});

describe("the PEP store", () => {
  test("a write changes only its own Space", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = {
        async send_raw_iq(xml: string) {
          sent.push(xml);
          return '<iq type="result"/>';
        },
      };
      const store = pepJoinedSpaceStore(xmpp, "alice@home.test");
      await store.put({ serviceJid: "spaces.a.test", node: "main", follow: false });
      await store.remove({ serviceJid: "spaces.b.test", node: "main" });
      expect(sent).toHaveLength(2);
      expect(sent[0]).toContain('<item id="xmpp:spaces.a.test?;node=main">');
      expect(sent[0]).not.toContain("spaces.b.test");
      expect(sent[1]).toContain('<item id="xmpp:spaces.b.test?;node=main">');
      expect(sent[1]).not.toContain("spaces.a.test");
    });
  });

  test("the first load moves a list of earlier versions into the node, then clears it", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = {
        async send_raw_iq(xml: string) {
          sent.push(xml);
          if (xml.includes(`<items node="${NODE}">`)) {
            return itemsXml([{ id: "xmpp:spaces.a.test?;node=main", space: 'service="spaces.a.test" node="main"' }]);
          }
          if (xml.includes("jabber:iq:private") && xml.includes('type="get"')) {
            return legacyXml(
              '<space service="spaces.a.test" node="main" follow="true"/>'
              + '<space service="spaces.b.test" node="eng" name="Eng" follow="true"/>',
            );
          }
          return '<iq type="result"/>';
        },
      };
      const loaded = await pepJoinedSpaceStore(xmpp, "alice@home.test").load();
      // The node's own entry wins over the old document's copy of it.
      expect(loaded).toEqual([
        { serviceJid: "spaces.a.test", node: "main", follow: false },
        { serviceJid: "spaces.b.test", node: "eng", name: "Eng", follow: true },
      ]);
      const publishes = sent.filter((xml) => xml.includes("<publish "));
      expect(publishes).toHaveLength(1);
      expect(publishes[0]).toContain('<item id="xmpp:spaces.b.test?;node=eng">');
      const clear = sent.findIndex((xml) => xml.includes("jabber:iq:private") && xml.includes('type="set"'));
      expect(clear).toBeGreaterThan(sent.indexOf(publishes[0]!));
    });
  });

  test("the old document is kept when moving it fails, and moved again next time", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = {
        async send_raw_iq(xml: string) {
          sent.push(xml);
          if (xml.includes(`<items node="${NODE}">`)) throw stanzaError("item-not-found");
          if (xml.includes("jabber:iq:private") && xml.includes('type="get"')) {
            return legacyXml('<space service="spaces.b.test" node="eng" follow="true"/>');
          }
          if (xml.includes("<publish ")) throw stanzaError("remote-server-timeout", "wait");
          return '<iq type="result"/>';
        },
      };
      const loaded = await pepJoinedSpaceStore(xmpp, "alice@home.test").load();
      expect(loaded).toEqual([{ serviceJid: "spaces.b.test", node: "eng", follow: true }]);
      expect(sent.some((xml) => xml.includes("jabber:iq:private") && xml.includes('type="set"'))).toBe(false);
    });
  });
});

describe("unfollows kept on this device", () => {
  test("are stored per account and survive a reload", () => {
    const storage = memoryStorage();
    const first = localSpaceUnfollowIntentStore("Alice@home.test", storage);
    first.save([{ serviceJid: "spaces.a.test", node: "main", remoteDone: false, preferenceDone: true }]);
    const again = localSpaceUnfollowIntentStore("alice@home.test", storage);
    expect(again.load()).toEqual([{ serviceJid: "spaces.a.test", node: "main", remoteDone: false, preferenceDone: true }]);
    expect(localSpaceUnfollowIntentStore("bob@home.test", storage).load()).toEqual([]);
    again.save([]);
    expect(storage.values.size).toBe(0);
  });

  test("unreadable or unavailable storage reads as nothing kept", () => {
    const storage = memoryStorage();
    storage.setItem("waddle:space-unfollow-intents:alice@home.test", "{not json");
    expect(localSpaceUnfollowIntentStore("alice@home.test", storage).load()).toEqual([]);
    const missing = localSpaceUnfollowIntentStore("alice@home.test", null);
    expect(missing.load()).toEqual([]);
    expect(() => missing.save([{ serviceJid: "spaces.a.test", node: "main", remoteDone: true, preferenceDone: false }])).not.toThrow();
  });
});

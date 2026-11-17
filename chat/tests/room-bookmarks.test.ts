import { describe, expect, test } from "bun:test";
import {
  BookmarkNodeConfigError,
  buildPublishRoomBookmarkIq,
  buildRetractRoomBookmarkIq,
  fetchRoomBookmark,
  findRoomBookmarks,
  parseRoomBookmarkItem,
  removeRoomBookmark,
  saveRoomBookmark,
  turnOffRoomBookmarkAutojoin,
} from "../src/lib/xmpp/room-bookmarks";
import { withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

const OWN = "alice@home.test";
const ROOM = "dev@chat.remote.test";

function withXml(run: () => Promise<void>): Promise<void> {
  return withFakeDomParser(() => withFakeXmlDocument(run));
}

function bookmarkItemsXml(conference: string): string {
  return `<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="urn:xmpp:bookmarks:1"><item id="${ROOM}">${conference}</item></items></pubsub></iq>`;
}

function stanzaError(condition: string, errorType = "cancel"): Error {
  return Object.assign(new Error(condition), { condition, errorType });
}

describe("XEP-0402 bookmark requests", () => {
  test("a new bookmark is published under the canonical room JID with XEP-0402 publish-options", async () => {
    await withXml(async () => {
      const xml = buildPublishRoomBookmarkIq("p1", { roomJid: "Dev@Chat.Remote.TEST.", name: "Dev", autojoin: true });
      expect(xml).toContain('<item id="dev@chat.remote.test">');
      expect(xml).toContain('<conference xmlns="urn:xmpp:bookmarks:1" name="Dev" autojoin="true">');
      expect(xml).toContain('<field var="pubsub#access_model"><value>whitelist</value></field>');
      expect(xml).toContain('<field var="pubsub#max_items"><value>max</value></field>');
      expect(xml).toContain('<field var="pubsub#send_last_published_item"><value>never</value></field>');
      expect(xml).toContain('<field var="pubsub#persist_items"><value>true</value></field>');
    });
  });

  test("autojoin false is written by leaving the attribute out", async () => {
    await withXml(async () => {
      const xml = buildPublishRoomBookmarkIq("p2", { roomJid: ROOM, autojoin: false });
      expect(xml).not.toContain("autojoin=");
    });
  });

  test("a removal retracts the item and notifies the user's other clients", async () => {
    await withXml(async () => {
      const xml = buildRetractRoomBookmarkIq("r1", ROOM);
      expect(xml).toContain('<retract node="urn:xmpp:bookmarks:1" notify="true">');
      expect(xml).toContain(`<item id="${ROOM}">`);
    });
  });

  test("a stored bookmark is read with its autojoin and name", async () => {
    await withFakeDomParser(async () => {
      const doc = new DOMParser().parseFromString(
        bookmarkItemsXml('<conference xmlns="urn:xmpp:bookmarks:1" name="Dev" autojoin="1"/>'),
        "text/xml",
      );
      expect(parseRoomBookmarkItem(doc, ROOM)).toMatchObject({ roomJid: ROOM, name: "Dev", autojoin: true });
      expect(parseRoomBookmarkItem(doc, "other@chat.remote.test")).toBeNull();
    });
  });
});

describe("XEP-0402 bookmark edits", () => {
  test("an edit keeps the stored nick, password and extensions and changes only autojoin", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = {
        async send_raw_iq(xml: string) {
          sent.push(xml);
          if (xml.includes('type="get"')) {
            return bookmarkItemsXml(
              '<conference xmlns="urn:xmpp:bookmarks:1" name="Dev" autojoin="true"><nick>Puck</nick><password>s3cret</password><extensions><notify xmlns="urn:xmpp:notification-settings:1"><never/></notify></extensions></conference>',
            );
          }
          return '<iq type="result"/>';
        },
      };
      const saved = await saveRoomBookmark(xmpp, OWN, { roomJid: ROOM, autojoin: false });
      expect(saved).toEqual({ roomJid: ROOM, name: "Dev", autojoin: false });
      const publish = sent.find((xml) => xml.includes("<publish "));
      expect(publish).toBeDefined();
      expect(publish).toContain("<nick>Puck</nick>");
      expect(publish).toContain("<password>s3cret</password>");
      expect(publish).toContain('<notify xmlns="urn:xmpp:notification-settings:1">');
      expect(publish).not.toContain("autojoin=");
      expect(publish).toContain('name="Dev"');
    });
  });

  test("a misconfigured bookmark node is reported as such", async () => {
    await withXml(async () => {
      const xmpp = {
        async send_raw_iq(xml: string) {
          if (xml.includes('type="get"')) throw stanzaError("item-not-found");
          throw stanzaError("precondition-not-met", "cancel");
        },
      };
      await expect(saveRoomBookmark(xmpp, OWN, { roomJid: ROOM, autojoin: true })).rejects.toBeInstanceOf(BookmarkNodeConfigError);
    });
  });

  test("no bookmark node yet reads as no bookmark", async () => {
    await withXml(async () => {
      const xmpp = { async send_raw_iq() { throw stanzaError("item-not-found"); } };
      expect(await fetchRoomBookmark(xmpp, OWN, ROOM)).toBeNull();
    });
  });

  test("removing a bookmark that is already gone succeeds", async () => {
    await withXml(async () => {
      const xmpp = { async send_raw_iq() { throw stanzaError("item-not-found"); } };
      await expect(removeRoomBookmark(xmpp, OWN, ROOM)).resolves.toBeUndefined();
    });
  });

  test("other failures to remove are reported", async () => {
    await withXml(async () => {
      const xmpp = { async send_raw_iq() { throw stanzaError("internal-server-error", "wait"); } };
      await expect(removeRoomBookmark(xmpp, OWN, ROOM)).rejects.toMatchObject({ condition: "internal-server-error" });
    });
  });
});

describe("bookmarks written under another spelling of the room's address", () => {
  function spellingsXml(): string {
    return '<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="urn:xmpp:bookmarks:1">'
      + '<item id="Dev@Chat.Remote.TEST."><conference xmlns="urn:xmpp:bookmarks:1" name="Dev" autojoin="true"><nick>Puck</nick></conference></item>'
      + '<item id="other@chat.remote.test"><conference xmlns="urn:xmpp:bookmarks:1" name="Other"/></item>'
      + '</items></pubsub></iq>';
  }

  test("are found by the room's canonical address", async () => {
    await withFakeDomParser(async () => {
      const doc = new DOMParser().parseFromString(spellingsXml(), "text/xml");
      const found = findRoomBookmarks(doc, ROOM);
      expect(found.bookmark).toMatchObject({ roomJid: ROOM, name: "Dev", autojoin: true });
      expect(found.otherSpellings).toEqual(["Dev@Chat.Remote.TEST."]);
    });
  });

  test("an edit moves the bookmark to the canonical item and retracts the other spelling", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = {
        async send_raw_iq(xml: string) {
          sent.push(xml);
          return xml.includes('type="get"') ? spellingsXml() : '<iq type="result"/>';
        },
      };
      await saveRoomBookmark(xmpp, OWN, { roomJid: ROOM, autojoin: false });
      const publish = sent.find((xml) => xml.includes("<publish "));
      expect(publish).toContain(`<item id="${ROOM}">`);
      // What the other spelling stored is carried over.
      expect(publish).toContain("<nick>Puck</nick>");
      expect(publish).toContain('name="Dev"');
      const retracts = sent.filter((xml) => xml.includes("<retract "));
      expect(retracts).toHaveLength(1);
      expect(retracts[0]).toContain('<item id="Dev@Chat.Remote.TEST.">');
      expect(sent.some((xml) => xml.includes("other@chat.remote.test") && xml.includes("<retract "))).toBe(false);
    });
  });

  test("a removal retracts every spelling of the room, and only that room", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = {
        async send_raw_iq(xml: string) {
          sent.push(xml);
          return xml.includes('type="get"') ? spellingsXml() : '<iq type="result"/>';
        },
      };
      await removeRoomBookmark(xmpp, OWN, ROOM);
      const retracted = sent.filter((xml) => xml.includes("<retract ")).map((xml) => /<item id="([^"]+)">/.exec(xml)?.[1]);
      expect(retracted.sort()).toEqual(["Dev@Chat.Remote.TEST.", ROOM].sort());
    });
  });
});

describe("turning autojoin off", () => {
  function bookmarkNode(stored: string | null) {
    const sent: string[] = [];
    const xmpp = {
      async send_raw_iq(xml: string) {
        sent.push(xml);
        if (xml.includes('type="get"')) {
          if (stored === null) throw stanzaError("item-not-found");
          return bookmarkItemsXml(stored);
        }
        return '<iq type="result"/>';
      },
    };
    return { sent, xmpp };
  }

  test("a bookmark that joins at sign-in is rewritten without autojoin, keeping the rest", async () => {
    await withXml(async () => {
      const { sent, xmpp } = bookmarkNode('<conference xmlns="urn:xmpp:bookmarks:1" name="Dev" autojoin="true"><nick>Puck</nick></conference>');
      expect(await turnOffRoomBookmarkAutojoin(xmpp, OWN, ROOM)).toBe(true);
      const publish = sent.find((xml) => xml.includes("<publish "));
      expect(publish).not.toContain("autojoin=");
      expect(publish).toContain('name="Dev"');
      expect(publish).toContain("<nick>Puck</nick>");
    });
  });

  test("a room without a bookmark, or whose bookmark already stays put, is not written", async () => {
    await withXml(async () => {
      for (const stored of [null, '<conference xmlns="urn:xmpp:bookmarks:1" name="Dev"/>']) {
        const { sent, xmpp } = bookmarkNode(stored);
        expect(await turnOffRoomBookmarkAutojoin(xmpp, OWN, ROOM)).toBe(false);
        expect(sent.some((xml) => xml.includes("<publish "))).toBe(false);
      }
    });
  });
});

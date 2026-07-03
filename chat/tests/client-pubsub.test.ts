/**
 * Unit tests for the PubSub/PEP module extracted from
 * `BrowserXmppClient` (`src/lib/xmpp/client-pubsub.ts`): story
 * reaction XML parsing, XEP-0060 publish/retract IQ generation for
 * story reactions, and the typed-outcome contract of the XEP-0492
 * notification-mode wrappers — all against a fake WASM client.
 */
import { describe, expect, test } from "bun:test";
import {
  PubsubManager,
  parseStoryReactionItems,
  parseStoryReactionSummary,
  type PubsubWasmClient,
} from "../src/lib/xmpp/client-pubsub";
import { withFakeDomParser } from "./helpers/disco-xml";

const SELF = "alice@example.com";

function createManager(xmpp: PubsubWasmClient) {
  return new PubsubManager({
    sessionJid: () => SELF,
    requireConnectedXmpp: async () => xmpp,
  });
}

describe("story reaction XML parsing", () => {
  test("parseStoryReactionItems reads per-reactor emoji lists", async () => {
    await withFakeDomParser(async () => {
      const xml = `<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="urn:waddle:stories:attachments:s1"><item id="bob@example.com"><attachments xmlns="urn:xmpp:pubsub-attachments:1"><reactions><reaction>👍</reaction><reaction>🎉</reaction></reactions></attachments></item></items></pubsub></iq>`;

      const items = parseStoryReactionItems(xml);

      expect(items).toHaveLength(1);
      expect(items[0].jid).toBe("bob@example.com");
      expect(items[0].emojis).toEqual(["👍", "🎉"]);
    });
  });

  test("parseStoryReactionSummary reads per-emoji counts and the noticed count", async () => {
    await withFakeDomParser(async () => {
      const xml = `<message><event xmlns="http://jabber.org/protocol/pubsub#event"><summary xmlns="urn:xmpp:pubsub-attachments:summary:1"><reactions><reaction count="3">👍</reaction></reactions><noticed count="7"/></summary></event></message>`;

      const summary = parseStoryReactionSummary(xml);

      expect(summary.counts).toEqual({ "👍": 3 });
      expect(summary.noticedCount).toBe(7);
    });
  });

  test("both parsers degrade to empty results without a DOMParser", () => {
    expect(parseStoryReactionItems("<iq/>")).toEqual([]);
    expect(parseStoryReactionSummary("<iq/>")).toEqual({ counts: {}, reactors: {}, mine: [] });
  });
});

describe("PubsubManager story reactions", () => {
  test("publishStoryReactions publishes an item keyed by our bare JID", async () => {
    const iqs: string[] = [];
    const manager = createManager({
      send_raw_iq: async (xml) => {
        iqs.push(xml);
        return "<iq type='result'/>";
      },
    });

    await manager.publishStoryReactions("community.example.com", "s1", ["👍"]);

    expect(iqs).toHaveLength(1);
    expect(iqs[0]).toContain("<publish");
    expect(iqs[0]).toContain('item id="alice@example.com"');
    expect(iqs[0]).toContain("<reaction>👍</reaction>");
  });

  test("publishing an empty reaction set retracts the item instead", async () => {
    const iqs: string[] = [];
    const manager = createManager({
      send_raw_iq: async (xml) => {
        iqs.push(xml);
        return "<iq type='result'/>";
      },
    });

    await manager.publishStoryReactions("community.example.com", "s1", []);

    expect(iqs).toHaveLength(1);
    expect(iqs[0]).toContain("<retract");
    expect(iqs[0]).not.toContain("<publish");
  });
});

describe("PubsubManager XEP-0492 notification modes", () => {
  test("setRoomNotificationMode maps a missing binding and a thrown error to the typed error outcome", async () => {
    const withoutBinding = createManager({});
    expect(await withoutBinding.setRoomNotificationMode({ roomJid: "general@muc.example.com", mode: "never" }))
      .toEqual({ kind: "error" });

    const throwing = createManager({
      set_room_notification_mode: async () => {
        throw new Error("transport dropped");
      },
    });
    expect(await throwing.setRoomNotificationMode({ roomJid: "general@muc.example.com", mode: "never" }))
      .toEqual({ kind: "error" });
  });

  test("fetchUserBookmarks resolves empty on failure instead of rejecting", async () => {
    const manager = createManager({
      fetch_user_bookmarks: async () => {
        throw new Error("not ready");
      },
    });
    expect(await manager.fetchUserBookmarks()).toEqual([]);
  });
});

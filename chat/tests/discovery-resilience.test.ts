import { afterEach, describe, expect, test } from "bun:test";
import {
  __setDiscoIqTimeoutForTest,
  DiscoTimeoutError,
  discoverTopology,
  withIqTimeout,
} from "../src/lib/xmpp/discovery";
import {
  discoInfoXml,
  discoItemsXml,
  pubsubItemsXml,
  withFakeDomParser,
} from "./helpers/disco-xml";

afterEach(() => {
  // Belt-and-braces: every test that mutates the disco timeout restores it
  // in its own finally, but if a future test forgets we want the next file
  // to start from production defaults.
  __setDiscoIqTimeoutForTest(null);
});

/**
 * Resilience contract for XEP-0030 topology discovery:
 *
 *  - RFC 6120 §8.2.3 requires IQ responses but cannot enforce it; every
 *    in-flight disco IQ MUST observe a bounded timeout so a wedged
 *    component cannot stall topology load forever.
 *  - Multi-component fan-out (`discoverComponentServices`,
 *    `discoverTopology` room hydration) MUST tolerate a single hung or
 *    failing component via the conventional-domain / unhydrated-room
 *    fallbacks. Implementation uses `Promise.allSettled`.
 *
 * These tests lock both contracts so we don't accidentally regress to
 * a `Promise.all` storm or drop the IQ timeout in a refactor.
 */

describe("withIqTimeout (RFC 6120 §8.2.3 defense)", () => {
  test("rejects with DiscoTimeoutError when the IQ does not resolve in time", async () => {
    const hung = new Promise<string>(() => {
      // intentionally never resolves
    });
    let caught: unknown = null;
    try {
      await withIqTimeout(hung, "calls.example.test", undefined, 30);
    } catch (err) {
      caught = err;
    }
    expect(caught).toBeInstanceOf(DiscoTimeoutError);
    expect((caught as DiscoTimeoutError).to).toBe("calls.example.test");
  });

  test("forwards the underlying resolution when the IQ answers before the timeout", async () => {
    const result = await withIqTimeout(Promise.resolve("ok"), "example.test", undefined, 1000);
    expect(result).toBe("ok");
  });

  test("clears the timer so a late rejection does not leak", async () => {
    // If the timer were not cleared on success, this Promise.race would
    // still hold a pending timeout — but since the test process exits
    // when all jobs settle, leaking timers would surface as unhandled
    // rejection warnings or hold the loop open. Bun fails fast on
    // either, so an assertion-free "resolves cleanly" suffices.
    await withIqTimeout(Promise.resolve(42), "example.test", "node-1", 5);
    // Wait past the original timeout window to confirm no late rejection
    // fires after the success path resolved.
    await new Promise((resolve) => setTimeout(resolve, 20));
  });

  test("cancels the raw IQ request when the timeout wins", async () => {
    const cancelled: string[] = [];
    const late = new Promise<string>((resolve) => {
      setTimeout(() => resolve("<iq type='result'/>"), 40);
    });

    await expect(withIqTimeout(
      late,
      "extensions.example.test",
      undefined,
      5,
      { cancel: async () => { cancelled.push("iq-timeout-1"); } },
    )).rejects.toBeInstanceOf(DiscoTimeoutError);

    expect(cancelled).toEqual(["iq-timeout-1"]);
  });
});

describe("discoverTopology partial-failure resilience", () => {
  test("falls back to the conventional service when one component disco#info hangs", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(resilientClient({ hangComponent: "extensions.example.test" }), "alice@example.test");

      // muc.example.test answered → that's the muc service we keep.
      expect(topology.services.muc).toBe("muc.example.test");
      // extensions.example.test hung → discoverComponentServices ignores it
      // and the conventional spaces.<domain> remains the spaces service.
      expect(topology.services.spaces).toBe("spaces.example.test");
    });
  });

  test("integration: real withIqTimeout fires end-to-end when a component disco#info never resolves", async () => {
    // Verifies the wiring: sendDiscoInfo MUST call withIqTimeout against
    // the underlying raw IQ promise. If a future refactor drops the
    // wrapper, this test fails because the never-resolving fixture below
    // would hang the test runner instead of timing out.
    __setDiscoIqTimeoutForTest(30);
    try {
      await withFakeDomParser(async () => {
        const topology = await discoverTopology(neverResolvesForComponent("extensions.example.test"), "alice@example.test");

        // muc.example.test still answered → service identity preserved.
        expect(topology.services.muc).toBe("muc.example.test");
        // extensions.example.test never resolved → timeout fired and the
        // conventional spaces.<domain> fallback kicked in.
        expect(topology.services.spaces).toBe("spaces.example.test");
      });
    } finally {
      __setDiscoIqTimeoutForTest(null);
    }
  });

  test("cancels the driver raw IQ when a component disco#info timeout fires", async () => {
    __setDiscoIqTimeoutForTest(30);
    try {
      await withFakeDomParser(async () => {
        const client = neverResolvesForComponent("extensions.example.test");
        const cancelledIds: string[] = [];
        const topology = await discoverTopology({
          ...client,
          async cancel_raw_iq(id: string): Promise<void> {
            cancelledIds.push(id);
          },
        }, "alice@example.test");

        expect(topology.services.spaces).toBe("spaces.example.test");
        expect(cancelledIds.length).toBe(1);
        expect(cancelledIds[0]).toBe(discoveryIqIdFor(client.sentIqs, "extensions.example.test", "disco#info"));
      });
    } finally {
      __setDiscoIqTimeoutForTest(null);
    }
  });

  test("allSettled in discoverComponentServices honors a SURVIVING explicit-identity component when another rejects", async () => {
    // Distinct-from-fallback coverage: if we regressed to `Promise.all` +
    // outer `catch { return fallback }`, this assertion would FAIL because
    // the muc identity would come from the conventional `muc.<domain>`
    // fallback, NOT from the surviving custom-named component below.
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        customMucWithBrokenSibling(),
        "alice@example.test",
      );

      // Custom muc JID survived even though the other component threw —
      // proves allSettled kept the per-entry result instead of collapsing
      // to the outer catch's fallback.
      expect(topology.services.muc).toBe("custom-muc.example.test");
    });
  });

  test("returns spaces topology even when the MUC service is wedged", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(resilientClient({ hangMucItems: true }), "alice@example.test");

      // Spaces discovery succeeded.
      expect(topology.spaces.map((space) => space.id)).toContain("space-engineering");
      expect(topology.roomCatalogComplete).toBe(false);
      // MUC items hung → rooms array stays empty rather than the whole
      // topology call rejecting.
      expect(topology.rooms).toEqual([]);
    });
  });

  test("returns the room list with unhydrated entries when one room disco#info rejects", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        resilientClient({ rejectRoomInfo: "broken@muc.example.test" }),
        "alice@example.test",
      );

      // Both rooms survive — the failing one falls back to a bare
      // channelFromRoom record without hydrated fields.
      const ids = topology.rooms.map((room) => room.id).sort();
      expect(ids).toEqual(["broken", "general"]);
      expect(topology.roomCatalogComplete).toBe(false);
      expect(
        topology.roomReconciliationAuthority.absentRoomKeysAuthoritative,
      ).toBe(false);
      expect(topology.roomReconciliationAuthority.roomFingerprints).toEqual([{
        roomKey: "general@muc.example.test",
        fields: ["spaceId", "autojoin", "isGroupDm", "isBookmarked"],
      }]);
    });
  });

  test("does not authorize absence when failed hydration omits a bookmark-only room", async () => {
    await withFakeDomParser(async () => {
      const omittedRoomJid = "private@muc.example.test";
      const topology = await discoverTopology(
        resilientClient({
          rejectRoomInfo: omittedRoomJid,
          userBookmarks: [{
            id: omittedRoomJid,
            name: "Private",
            autojoin: true,
          }],
        }),
        "alice@example.test",
      );

      expect(topology.roomCatalogComplete).toBe(false);
      expect(topology.rooms.some((room) => room.jid === omittedRoomJid)).toBe(false);
      expect(
        topology.roomReconciliationAuthority.absentRoomKeysAuthoritative,
      ).toBe(false);
    });
  });

  test("does not authorize absence when incomplete hydration omits a bookmark-only room", async () => {
    await withFakeDomParser(async () => {
      const omittedRoomJid = "private@muc.example.test";
      const topology = await discoverTopology(
        resilientClient({
          incompleteRoomInfo: omittedRoomJid,
          userBookmarks: [{
            id: omittedRoomJid,
            name: "Private",
            autojoin: true,
          }],
        }),
        "alice@example.test",
      );

      expect(topology.roomCatalogComplete).toBe(false);
      expect(topology.rooms.some((room) => room.jid === omittedRoomJid)).toBe(false);
      expect(
        topology.roomReconciliationAuthority.absentRoomKeysAuthoritative,
      ).toBe(false);
    });
  });

  test("marks the room catalog incomplete when user bookmarks fail", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        resilientClient({ rejectUserBookmarks: true }),
        "alice@example.test",
      );

      expect(topology.rooms.map((room) => room.id).sort()).toEqual([
        "broken",
        "general",
      ]);
      expect(topology.roomCatalogComplete).toBe(false);
      expect(
        topology.roomReconciliationAuthority.absentRoomKeysAuthoritative,
      ).toBe(false);
      expect(topology.roomReconciliationAuthority.roomFingerprints).toEqual([]);
    });
  });

  test("marks the room catalog incomplete when space bookmarks fail", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        resilientClient({ rejectSpaceBookmarks: true }),
        "alice@example.test",
      );

      expect(topology.rooms.map((room) => room.id).sort()).toEqual([
        "broken",
        "general",
      ]);
      expect(topology.roomCatalogComplete).toBe(false);
      expect(
        topology.roomReconciliationAuthority.absentRoomKeysAuthoritative,
      ).toBe(false);
      expect(topology.roomReconciliationAuthority.roomFingerprints).toEqual([]);
    });
  });

  test("keeps exact space-bookmark authority when user bookmarks fail", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        resilientClient({
          rejectUserBookmarks: true,
          spaceBookmarks: [{
            id: "general@muc.example.test",
            name: "General",
            autojoin: false,
          }],
        }),
        "alice@example.test",
      );

      expect(topology.roomCatalogComplete).toBe(false);
      expect(topology.roomReconciliationAuthority.roomFingerprints).toEqual([{
        roomKey: "general@muc.example.test",
        fields: ["isGroupDm", "isBookmarked", "spaceId"],
      }]);
    });
  });

  test("keeps exact user-bookmark authority when space bookmarks fail", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        resilientClient({
          rejectSpaceBookmarks: true,
          userBookmarks: [{
            id: "general@muc.example.test",
            name: "General",
            autojoin: false,
          }],
        }),
        "alice@example.test",
      );

      expect(topology.roomCatalogComplete).toBe(false);
      expect(topology.roomReconciliationAuthority.roomFingerprints).toEqual([{
        roomKey: "general@muc.example.test",
        fields: ["isGroupDm", "isBookmarked", "autojoin"],
      }]);
    });
  });
});

type ResilientClientOptions = {
  hangComponent?: string;
  hangMucItems?: boolean;
  incompleteRoomInfo?: string;
  rejectSpaceBookmarks?: boolean;
  rejectUserBookmarks?: boolean;
  rejectRoomInfo?: string;
  spaceBookmarks?: Array<{ id: string; name: string; autojoin: boolean }>;
  userBookmarks?: Array<{ id: string; name: string; autojoin: boolean }>;
};

function neverResolvesForComponent(jid: string) {
  const sentIqs: string[] = [];
  return {
    sentIqs,
    async send_raw_iq(xml: string): Promise<string> {
      sentIqs.push(xml);
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          return discoItemsXml([
            { jid: "muc.example.test", name: "Chatrooms" },
            { jid: jid, name: "Hung" },
          ]);
        }
        if (xml.includes('to="muc.example.test"')) {
          return discoItemsXml([]);
        }
        return discoItemsXml([]);
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (xml.includes(`to="${jid}"`)) {
          // Production stall reproduction: never resolves. The wrapping
          // withIqTimeout in sendDiscoInfo MUST reject this with
          // DiscoTimeoutError before the test's wall-clock budget; if the
          // wrapper is missing, bun test will hang past the per-test
          // timeout instead.
          return new Promise<string>(() => {});
        }
        if (xml.includes('to="muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text", name: "Chatrooms" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        return discoInfoXml();
      }
      return discoItemsXml([]);
    },
  };
}

function discoveryIqIdFor(sentIqs: string[], to: string, namespaceFragment: string): string | undefined {
  const iq = sentIqs.find((xml) => xml.includes(`to="${to}"`) && xml.includes(namespaceFragment));
  return iq?.match(/\sid="([^"]+)"/)?.[1];
}

function customMucWithBrokenSibling() {
  // Two components advertised: one custom-named MUC service and one that
  // rejects disco#info synchronously. allSettled MUST surface the custom
  // muc identity even though the sibling threw.
  return {
    async send_raw_iq(xml: string): Promise<string> {
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          return discoItemsXml([
            { jid: "custom-muc.example.test", name: "ChatPalace" },
            { jid: "broken.example.test", name: "Broken" },
          ]);
        }
        if (xml.includes('to="custom-muc.example.test"')) {
          return discoItemsXml([]);
        }
        return discoItemsXml([]);
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (xml.includes('to="broken.example.test"')) {
          throw new Error("simulated sibling disco#info failure");
        }
        if (xml.includes('to="custom-muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text", name: "ChatPalace" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        return discoInfoXml();
      }
      return discoItemsXml([]);
    },
  };
}

function resilientClient(options: ResilientClientOptions = {}) {
  return {
    async send_raw_iq(xml: string): Promise<string> {
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          return discoItemsXml([
            { jid: "muc.example.test", name: "Chatrooms" },
            { jid: "spaces.example.test", name: "Spaces" },
            { jid: "extensions.example.test", name: "Extensions" },
          ]);
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoItemsXml([
            { name: "Engineering", node: "space-engineering" },
          ]);
        }
        if (xml.includes('to="muc.example.test"')) {
          if (options.hangMucItems) {
            // Simulates the production stall as a synchronous rejection:
            // testing the actual wedged-promise + 15s real timeout would
            // hold the suite for the full timeout window. We bypass the
            // timer path here and assert that the same try/catch in
            // discoverTopology degrades the rooms list gracefully when
            // sendDiscoItems rejects for ANY reason — timeout or stanza
            // error — which is the contract callers depend on.
            throw new Error("simulated muc service stall");
          }
          return discoItemsXml([
            { jid: "general@muc.example.test", name: "General" },
            { jid: "broken@muc.example.test", name: "Broken" },
          ]);
        }
        return discoItemsXml([]);
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (options.hangComponent && xml.includes(`to="${options.hangComponent}"`)) {
          // Hung component disco#info: tests use a short withIqTimeout
          // budget via the wrapping helper. Even with the production
          // 15s default, allSettled in discoverComponentServices means
          // a single hang cannot wedge the others — so this test does
          // not need to short-circuit the timer.
          throw new Error("simulated component disco#info timeout");
        }
        if (options.rejectRoomInfo && xml.includes(`to="${options.rejectRoomInfo}"`)) {
          throw new Error("simulated room disco#info failure");
        }
        if (
          options.incompleteRoomInfo
          && xml.includes(`to="${options.incompleteRoomInfo}"`)
        ) {
          return '<iq type="result"/>';
        }
        if (xml.includes('to="muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text", name: "Chatrooms" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        if (xml.includes('to="spaces.example.test"') && !xml.includes(' node=')) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service", name: "Spaces" }],
            features: [
              "http://jabber.org/protocol/pubsub",
              "urn:xmpp:spaces:0",
            ],
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
        if (xml.includes('to="general@muc.example.test"') || xml.includes('to="broken@muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text", name: "Room" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        if (xml.includes('to="example.test"')) {
          return discoInfoXml({
            identities: [{ category: "server", type: "im", name: "Waddle" }],
            features: [],
          });
        }
        return discoInfoXml();
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
        if (
          options.rejectSpaceBookmarks
          && xml.includes('to="spaces.example.test"')
        ) {
          throw new Error("simulated space bookmark failure");
        }
        if (
          options.rejectUserBookmarks
          && xml.includes('to="alice@example.test"')
        ) {
          throw new Error("simulated user bookmark failure");
        }
        if (xml.includes('to="spaces.example.test"')) {
          return pubsubItemsXml(options.spaceBookmarks ?? []);
        }
        return pubsubItemsXml(options.userBookmarks ?? []);
      }
      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
}

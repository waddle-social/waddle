import { describe, expect, test } from "bun:test";
import {
  applyDiscoInfoToChannel,
  discoverTopology,
  pinPermissionFromDiscoFields,
  type DiscoInfoData,
} from "../src/lib/xmpp/discovery";
import type { DiscoveredChannel } from "../src/lib/xmpp/types";
import {
  discoInfoXml,
  discoItemsXml,
  pubsubItemsXml,
  withFakeDomParser,
} from "./helpers/disco-xml";

describe("pinPermissionFromDiscoFields (#422)", () => {
  test("extracts 'anyone' when the disco field carries that value", () => {
    const fields = new Map([
      ["FORM_TYPE", "urn:waddle:room:0"],
      ["waddle#channel_type", "text"],
      ["urn:waddle:roomconfig:pinpermission", "anyone"],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBe("anyone");
  });

  test("extracts 'admins-only' when the disco field carries that value", () => {
    const fields = new Map([
      ["urn:waddle:roomconfig:pinpermission", "admins-only"],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBe("admins-only");
  });

  test("returns undefined when the field is absent", () => {
    const fields = new Map([
      ["FORM_TYPE", "urn:waddle:room:0"],
      ["waddle#channel_type", "text"],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBeUndefined();
  });

  test("returns undefined when the field carries an unknown value", () => {
    const fields = new Map([
      ["urn:waddle:roomconfig:pinpermission", "open-house"],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBeUndefined();
  });

  test("returns undefined when the field is empty", () => {
    const fields = new Map([
      ["urn:waddle:roomconfig:pinpermission", ""],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBeUndefined();
  });
});

/** #422 hydration-path coverage: `applyDiscoInfoToChannel` is the
 * pure transform that maps a parsed disco-info payload onto a
 * `DiscoveredChannel`. `hydrateRoomInfo` is the thin IO wrapper that
 * calls `sendDiscoInfo` (DOMParser path, browser-only) and then
 * delegates here. By exercising this transform directly we lock the
 * stamping contract that `loadStructure` consumes — without needing a
 * DOM polyfill in bun-test. */
describe("applyDiscoInfoToChannel pinPermission (#422)", () => {
  const baseRoom: DiscoveredChannel = {
    id: "general",
    name: "General",
    jid: "general@conference.example.net",
    channelType: "text",
    position: 0,
  };

  function infoWithPin(value: string): DiscoInfoData {
    return {
      features: ["http://jabber.org/protocol/muc"],
      identities: [{ category: "conference", type: "text", name: "General" }],
      fields: new Map([
        ["FORM_TYPE", "urn:waddle:room:0"],
        ["waddle#channel_type", "text"],
        ["urn:waddle:roomconfig:pinpermission", value],
      ]),
    };
  }

  test("stamps pinPermission='anyone' onto the channel", () => {
    const hydrated = applyDiscoInfoToChannel(baseRoom, infoWithPin("anyone"));
    expect(hydrated.pinPermission).toBe("anyone");
  });

  test("stamps pinPermission='admins-only' onto the channel", () => {
    const hydrated = applyDiscoInfoToChannel(baseRoom, infoWithPin("admins-only"));
    expect(hydrated.pinPermission).toBe("admins-only");
  });

  test("leaves pinPermission undefined when the disco field is absent", () => {
    const hydrated = applyDiscoInfoToChannel(baseRoom, {
      features: ["http://jabber.org/protocol/muc"],
      identities: [],
      fields: new Map(),
    });
    expect(hydrated.pinPermission).toBeUndefined();
  });
});

describe("discoverTopology spaces service discovery", () => {
  test("uses the conventional Spaces service instead of the generic extension pubsub service", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(topologyClient(), "alice@example.test");

      expect(topology.services.spaces).toBe("spaces.example.test");
      expect(topology.spaces.map((space) => space.id)).toEqual(["space-polls", "space-engineering"]);
      expect(topology.spaces.map((space) => space.name)).toEqual(["Polls", "Engineering"]);
    });
  });

  test("falls back to the conventional Spaces service when root discovery returns no services", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(topologyClient({ emptyRootItems: true }), "alice@example.test");

      expect(topology.services.spaces).toBe("spaces.example.test");
      expect(topology.spaces.map((space) => space.id)).toEqual(["space-polls", "space-engineering"]);
    });
  });

  test("falls back to the conventional Spaces service when only the extension pubsub service is advertised", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(topologyClient({ omitSpacesServiceItem: true }), "alice@example.test");

      expect(topology.services.spaces).toBe("spaces.example.test");
      expect(topology.spaces.map((space) => space.id)).toEqual(["space-polls", "space-engineering"]);
    });
  });

  test("does not render root service disco items as empty Spaces groups", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(topologyClient({ rootAdvertisesSpacesService: true }), "alice@example.test");

      expect(topology.services.spaces).toBe("example.test");
      expect(topology.spaces).toEqual([]);
    });
  });
});

type TopologyClientOptions = {
  emptyRootItems?: boolean;
  omitSpacesServiceItem?: boolean;
  rootAdvertisesSpacesService?: boolean;
};

function topologyClient(options: TopologyClientOptions = {}) {
  return {
    async send_raw_iq(xml: string): Promise<string> {
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          if (options.emptyRootItems) return discoItemsXml([]);
          const rootItems = [
            { jid: "extensions.example.test", name: "Extensions" },
            ...(options.omitSpacesServiceItem ? [] : [{ jid: "spaces.example.test", name: "Spaces" }]),
            { jid: "muc.example.test", name: "Chatrooms" },
            ...(options.rootAdvertisesSpacesService ? [{ jid: "example.test", name: "Waddle" }] : []),
          ];
          return discoItemsXml(rootItems);
        }
        if (xml.includes('to="extensions.example.test"')) {
          return discoItemsXml([
            { jid: "extensions.example.test", name: "Polls", node: "urn:waddle:extension:1:route:decision-polls:polls" },
            { jid: "extensions.example.test", name: "Saved Links", node: "urn:waddle:extension:1:route:link-board:saved-links" },
          ]);
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoItemsXml([
            { name: "Polls", node: "space-polls" },
            { name: "Engineering", node: "space-engineering" },
          ]);
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
        if (xml.includes('to="spaces.example.test"') && !xml.includes(' node=')) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service", name: "Spaces" }],
            features: [
              "http://jabber.org/protocol/pubsub",
              "http://jabber.org/protocol/pubsub#create-nodes",
              "http://jabber.org/protocol/pubsub#meta-data",
              "http://jabber.org/protocol/pubsub#retrieve-items",
            ],
          });
        }
        if (xml.includes('to="extensions.example.test"')) {
          await new Promise((resolve) => setTimeout(resolve, 1));
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service", name: "Waddle Extensions" }],
            features: ["http://jabber.org/protocol/pubsub", "urn:waddle:extension:1"],
          });
        }
        if (xml.includes('to="example.test"')) {
          return discoInfoXml({
            identities: [{ category: "server", type: "im", name: "Waddle" }],
            features: options.rootAdvertisesSpacesService ? ["urn:xmpp:spaces:0"] : [],
          });
        }
        return discoInfoXml();
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
        return pubsubItemsXml([]);
      }
      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
}


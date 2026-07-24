import { describe, expect, mock, test } from "bun:test";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import { nullResumePersistence } from "../src/lib/xmpp/resume-persistence";
import {
  discoInfoXml,
  discoItemsXml,
  pubsubItemsXml,
  withFakeDomParser,
} from "./helpers/disco-xml";

const roomJid = "private@rooms.example.test";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.test",
    session_id: "s1",
    user_id: "u1",
    avatar_url: null,
    xmpp_localpart: "alice",
    xmpp_websocket_url: "wss://example.test/xmpp",
    is_expired: false,
    expires_at: null,
  } as WaddleSession;
}

type DiscoveryFixtureState = {
  mucCatalogAvailable: boolean;
  roomJids: string[];
  bookmarks: Array<{ id: string; name: string; autojoin: boolean }>;
  spaceBookmarks?: Array<{ id: string; name: string; autojoin: boolean }>;
  rejectSpaceBookmarks?: boolean;
  rejectUserBookmarks?: boolean;
  rejectRoomInfo?: string;
};

function createDiscoveryXmpp(
  state: DiscoveryFixtureState,
  joinRoom: ReturnType<typeof mock>,
) {
  return {
    join_room: joinRoom,
    async send_raw_iq(xml: string): Promise<string> {
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          return discoItemsXml([
            { jid: "spaces.example.test", name: "Spaces" },
            { jid: "muc.example.test", name: "Chatrooms" },
          ]);
        }
        if (xml.includes('to="spaces.example.test"')) {
          return state.spaceBookmarks || state.rejectSpaceBookmarks
            ? discoItemsXml([
                { name: "Engineering", node: "space-engineering" },
              ])
            : discoItemsXml([]);
        }
        if (xml.includes('to="muc.example.test"')) {
          if (!state.mucCatalogAvailable) {
            throw new Error("temporary MUC catalog failure");
          }
          return discoItemsXml(
            state.roomJids.map((jid) => ({
              jid,
              name: jid === roomJid ? "Private" : "Broken",
            })),
          );
        }
        return discoItemsXml([]);
      }

      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (
          state.rejectRoomInfo
          && xml.includes(`to="${state.rejectRoomInfo}"`)
        ) {
          throw new Error("temporary room hydration failure");
        }
        if (
          xml.includes('to="muc.example.test"')
          || state.roomJids.some((jid) => xml.includes(`to="${jid}"`))
          || state.bookmarks.some(({ id }) => xml.includes(`to="${id}"`))
        ) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        if (xml.includes('to="spaces.example.test"')) {
          if (xml.includes(' node=')) {
            return discoInfoXml({
              identities: [{ category: "pubsub", type: "leaf" }],
              features: ["http://jabber.org/protocol/pubsub", "urn:xmpp:spaces:0"],
              fields: {
                FORM_TYPE: "http://jabber.org/protocol/pubsub#meta-data",
                "pubsub#type": "urn:xmpp:spaces:0",
              },
            });
          }
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service" }],
            features: ["http://jabber.org/protocol/pubsub", "urn:xmpp:spaces:0"],
          });
        }
        return discoInfoXml({
          identities: [{ category: "server", type: "im" }],
        });
      }

      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
        if (xml.includes('to="spaces.example.test"')) {
          if (state.rejectSpaceBookmarks) {
            throw new Error("temporary space bookmark failure");
          }
          return pubsubItemsXml(state.spaceBookmarks ?? []);
        }
        if (
          state.rejectUserBookmarks
          && xml.includes('to="alice@example.test"')
        ) {
          throw new Error("temporary bookmark failure");
        }
        return pubsubItemsXml(state.bookmarks);
      }

      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
}

describe("BrowserXmppClient room discovery cache", () => {
  test("degraded refreshes preserve denial, room, and auto-join caches", async () => {
    await withFakeDomParser(async () => {
      const fixture: DiscoveryFixtureState = {
        mucCatalogAvailable: true,
        roomJids: [roomJid],
        bookmarks: [{ id: roomJid, name: "Private", autojoin: true }],
        rejectUserBookmarks: false,
      };
      const joinRoom = mock(async () => {
        throw new Error("a degraded refresh must not attempt a join");
      });
      const xmpp = createDiscoveryXmpp(fixture, joinRoom);
      const client = new BrowserXmppClient(session(), {
        ...nullResumePersistence,
        loadAutoJoinBlocks: () => [{
          roomJid,
          condition: "forbidden",
        }],
        saveAutoJoinBlocks: () => {},
        clearAutoJoinBlocks: () => {},
      });
      const state = client as unknown as {
        xmpp: typeof xmpp;
        connected: boolean;
        roomJidForChannel: (channelId: string) => string;
        autoJoinRoomJids: readonly string[];
      };
      state.xmpp = xmpp;
      state.connected = true;

      const complete = await client.discoverTopology();
      expect(complete.roomCatalogComplete).toBe(true);
      expect(state.roomJidForChannel("private")).toBe(roomJid);
      expect(state.autoJoinRoomJids).toEqual([roomJid]);

      fixture.rejectUserBookmarks = true;
      const bookmarkDegraded = await client.discoverTopology();

      expect(bookmarkDegraded.roomCatalogComplete).toBe(false);
      expect(bookmarkDegraded.rooms.map((room) => room.jid)).toEqual([roomJid]);
      expect(state.roomJidForChannel("private")).toBe(roomJid);
      expect(state.autoJoinRoomJids).toEqual([roomJid]);
      expect(joinRoom).not.toHaveBeenCalled();

      fixture.rejectUserBookmarks = false;
      fixture.mucCatalogAvailable = false;
      fixture.bookmarks = [];
      const degraded = await client.discoverTopology();

      expect(degraded.roomCatalogComplete).toBe(false);
      expect(degraded.rooms).toEqual([]);
      expect(state.roomJidForChannel("private")).toBe(roomJid);
      expect(state.autoJoinRoomJids).toEqual([roomJid]);
      expect(joinRoom).not.toHaveBeenCalled();
    });
  });

  for (const {
    degradedSource,
    expectedCachedAutoJoin,
    shouldUnblock,
  } of [
    {
      degradedSource: "sibling hydration",
      expectedCachedAutoJoin: false,
      shouldUnblock: true,
    },
    {
      degradedSource: "MUC items",
      expectedCachedAutoJoin: false,
      shouldUnblock: true,
    },
    {
      degradedSource: "blocked room hydration",
      expectedCachedAutoJoin: true,
      shouldUnblock: false,
    },
  ] as const) {
    test(`${shouldUnblock ? "an authoritative" : "an incomplete"} membership change ${
      shouldUnblock ? "unblocks" : "stays blocked"
    } through degraded ${degradedSource}`, async () => {
      await withFakeDomParser(async () => {
        const siblingRoomJid = "broken@rooms.example.test";
        const fixture: DiscoveryFixtureState = {
          mucCatalogAvailable: true,
          roomJids: [roomJid, siblingRoomJid],
          bookmarks: [],
        };
        const joinRoom = mock(async () => undefined);
        const xmpp = createDiscoveryXmpp(fixture, joinRoom);
        const client = new BrowserXmppClient(session(), {
          ...nullResumePersistence,
          loadAutoJoinBlocks: () => [{
            roomJid,
            condition: "forbidden",
          }],
        });
        const state = client as unknown as {
          xmpp: typeof xmpp;
          connected: boolean;
          autoJoinRoomJids: readonly string[];
        };
        state.xmpp = xmpp;
        state.connected = true;

        const baseline = await client.discoverTopology();
        expect(baseline.roomCatalogComplete).toBe(true);
        expect(client.listRoomAccessRequirements()).toHaveLength(1);
        joinRoom.mockClear();

        fixture.bookmarks = [{
          id: roomJid,
          name: "Private",
          autojoin: false,
        }];
        if (degradedSource === "MUC items") {
          fixture.mucCatalogAvailable = false;
        } else {
          fixture.rejectRoomInfo =
            degradedSource === "sibling hydration"
              ? siblingRoomJid
              : roomJid;
        }
        const changed = await client.discoverTopology();

        expect(changed.roomCatalogComplete).toBe(false);
        expect(
          changed.roomReconciliationAuthority.roomFingerprints.some(
            ({ roomKey }) => roomKey === roomJid,
          ),
        ).toBe(shouldUnblock);
        expect(client.listRoomAccessRequirements()).toHaveLength(
          shouldUnblock ? 0 : 1,
        );
        expect(state.autoJoinRoomJids.includes(roomJid)).toBe(
          expectedCachedAutoJoin,
        );
        expect(
          joinRoom.mock.calls.some(([joinedRoomJid]) =>
            joinedRoomJid === roomJid
          ),
        ).toBe(false);
      });
    });
  }

  test("an exact space-membership change unblocks despite failed user bookmarks", async () => {
    await withFakeDomParser(async () => {
      const fixture: DiscoveryFixtureState = {
        mucCatalogAvailable: true,
        roomJids: [roomJid],
        bookmarks: [],
      };
      const joinRoom = mock(async () => undefined);
      const xmpp = createDiscoveryXmpp(fixture, joinRoom);
      const client = new BrowserXmppClient(session(), {
        ...nullResumePersistence,
        loadAutoJoinBlocks: () => [{
          roomJid,
          condition: "forbidden",
        }],
      });
      const state = client as unknown as {
        xmpp: typeof xmpp;
        connected: boolean;
      };
      state.xmpp = xmpp;
      state.connected = true;

      await client.discoverTopology();
      expect(client.listRoomAccessRequirements()).toHaveLength(1);
      const accessEvents: Array<{ roomJid: string; state: string }> = [];
      client.onRoomAccessChanged((event) => accessEvents.push(event));
      joinRoom.mockClear();

      fixture.spaceBookmarks = [{
        id: roomJid,
        name: "Private",
        autojoin: true,
      }];
      fixture.rejectUserBookmarks = true;
      const changed = await client.discoverTopology();

      expect(changed.roomCatalogComplete).toBe(false);
      expect(changed.roomReconciliationAuthority.roomFingerprints).toEqual([{
        roomKey: roomJid,
        fields: ["isGroupDm", "isBookmarked", "spaceId"],
      }]);
      expect(client.listRoomAccessRequirements()).toEqual([]);
      expect(accessEvents).toContainEqual({ roomJid, state: "available" });
      expect(joinRoom).not.toHaveBeenCalled();
    });
  });

  test("an omitted bookmark-only room cannot clear denial or discovery caches", async () => {
    await withFakeDomParser(async () => {
      const fixture: DiscoveryFixtureState = {
        mucCatalogAvailable: true,
        roomJids: [roomJid],
        bookmarks: [{ id: roomJid, name: "Private", autojoin: true }],
      };
      const joinRoom = mock(async () => undefined);
      const xmpp = createDiscoveryXmpp(fixture, joinRoom);
      const client = new BrowserXmppClient(session(), {
        ...nullResumePersistence,
        loadAutoJoinBlocks: () => [{
          roomJid,
          condition: "forbidden",
        }],
      });
      const state = client as unknown as {
        xmpp: typeof xmpp;
        connected: boolean;
        roomJidForChannel: (channelId: string) => string;
        autoJoinRoomJids: readonly string[];
      };
      state.xmpp = xmpp;
      state.connected = true;

      await client.discoverTopology();
      expect(client.listRoomAccessRequirements()).toHaveLength(1);
      expect(state.roomJidForChannel("private")).toBe(roomJid);
      expect(state.autoJoinRoomJids).toEqual([roomJid]);
      const accessEvents: Array<{ roomJid: string; state: string }> = [];
      client.onRoomAccessChanged((event) => accessEvents.push(event));
      joinRoom.mockClear();

      fixture.roomJids = [];
      fixture.rejectRoomInfo = roomJid;
      const degraded = await client.discoverTopology();

      expect(degraded.roomCatalogComplete).toBe(false);
      expect(degraded.rooms).toEqual([]);
      expect(
        degraded.roomReconciliationAuthority.absentRoomKeysAuthoritative,
      ).toBe(false);
      expect(client.listRoomAccessRequirements()).toHaveLength(1);
      expect(state.roomJidForChannel("private")).toBe(roomJid);
      expect(state.autoJoinRoomJids).toEqual([roomJid]);
      expect(accessEvents).toEqual([]);
      expect(joinRoom).not.toHaveBeenCalled();
    });
  });
});

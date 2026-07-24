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

describe("BrowserXmppClient room discovery cache", () => {
  test("degraded refreshes preserve denial, room, and auto-join caches", async () => {
    await withFakeDomParser(async () => {
      let mucCatalogAvailable = true;
      let rejectUserBookmarks = false;
      const joinRoom = mock(async () => {
        throw new Error("a degraded refresh must not attempt a join");
      });
      const xmpp = {
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
              return discoItemsXml([]);
            }
            if (xml.includes('to="muc.example.test"')) {
              if (!mucCatalogAvailable) {
                throw new Error("temporary MUC catalog failure");
              }
              return discoItemsXml([{ jid: roomJid, name: "Private" }]);
            }
            return discoItemsXml([]);
          }

          if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
            if (xml.includes('to="muc.example.test"')) {
              return discoInfoXml({
                identities: [{ category: "conference", type: "text" }],
                features: ["http://jabber.org/protocol/muc"],
              });
            }
            if (xml.includes(`to="${roomJid}"`)) {
              return discoInfoXml({
                identities: [{ category: "conference", type: "text" }],
                features: ["http://jabber.org/protocol/muc"],
              });
            }
            if (xml.includes('to="spaces.example.test"')) {
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
            if (
              rejectUserBookmarks
              && xml.includes('to="alice@example.test"')
            ) {
              throw new Error("temporary bookmark failure");
            }
            return pubsubItemsXml(
              mucCatalogAvailable
                ? [{ id: roomJid, name: "Private", autojoin: true }]
                : [],
            );
          }

          throw new Error(`Unexpected IQ: ${xml}`);
        },
      };
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

      rejectUserBookmarks = true;
      const bookmarkDegraded = await client.discoverTopology();

      expect(bookmarkDegraded.roomCatalogComplete).toBe(false);
      expect(bookmarkDegraded.rooms.map((room) => room.jid)).toEqual([roomJid]);
      expect(state.roomJidForChannel("private")).toBe(roomJid);
      expect(state.autoJoinRoomJids).toEqual([roomJid]);
      expect(joinRoom).not.toHaveBeenCalled();

      rejectUserBookmarks = false;
      mucCatalogAvailable = false;
      const degraded = await client.discoverTopology();

      expect(degraded.roomCatalogComplete).toBe(false);
      expect(degraded.rooms).toEqual([]);
      expect(state.roomJidForChannel("private")).toBe(roomJid);
      expect(state.autoJoinRoomJids).toEqual([roomJid]);
      expect(joinRoom).not.toHaveBeenCalled();
    });
  });
});

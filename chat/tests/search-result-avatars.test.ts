import { afterEach, describe, expect, test } from "bun:test";
import { TypedEventBus, type ClientEvents } from "../src/lib/xmpp/client-events";
import { MamPager, type MamWasmClient } from "../src/lib/xmpp/client-mam";
import { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";
import { authorAvatarJid, occupantJidDirectory } from "../src/lib/avatars/author-jid";
import type { WasmArchivedMessage, WasmMamPage } from "../src/lib/xmpp/wasm-types";

// The viewer's account: mixed-case localpart, as an OIDC username can be.
const SELF = "Rawkode@example.com";
const SELF_FULL = `${SELF}/web`;
const PEER = "bob@example.com";
const ROOM = "general@muc.example.com";

afterEach(() => occupantJidDirectory.clear());

function page(messages: WasmArchivedMessage[]): WasmMamPage {
  return { messages, is_complete: true };
}

function archived(overrides: Partial<WasmArchivedMessage> & { mam_id: string }): WasmArchivedMessage {
  return {
    id: `msg-${overrides.mam_id}`,
    body: "matching",
    message_type: "chat",
    timestamp: "2026-09-27T10:00:00Z",
    ...overrides,
  } as WasmArchivedMessage;
}

function pager(xmpp: MamWasmClient) {
  return new MamPager({
    sessionJid: () => SELF,
    fullJid: () => SELF_FULL,
    trustedMediaOrigin: () => null,
    currentRoom: () => null,
    catchup: new ReconnectCatchup(),
    events: new TypedEventBus<ClientEvents>(),
    emitError: () => undefined,
    requireConnectedXmpp: async () => xmpp,
    ensureRoomReady: async () => undefined,
    roomJidForChannel: (channelId) => `${channelId}@muc.example.com`,
    isCurrentConnected: (candidate) => candidate === xmpp,
    classifyMucPm: (message) => {
      const counterpart = (message.from ?? "").toLowerCase().startsWith(SELF.toLowerCase()) ? (message.to ?? "") : (message.from ?? "");
      const [bare, ...rest] = counterpart.split("/");
      const nick = rest.join("/");
      if (!nick || bare !== ROOM) return undefined;
      return { occupantJid: counterpart, nick };
    },
    isMucPmPeer: (peerJid) => peerJid.startsWith(`${ROOM}/`),
  });
}

describe("search hits resolve avatars like their timeline rows", () => {
  test("own DM hits show our face even when the nick differs in case from the username", async () => {
    const xmpp: MamWasmClient = {
      search_dm_history: async () => page([
        archived({ mam_id: "mine", from: SELF_FULL, to: PEER, timestamp: "2026-09-27T10:00:00Z" }),
        archived({ mam_id: "theirs", from: `${PEER}/phone`, to: SELF, timestamp: "2026-09-27T10:01:00Z" }),
      ]),
    };
    const results = await pager(xmpp).searchDmMessages(PEER, "matching");
    const byArchive = new Map(results.map((result) => [result.archiveId, result]));

    expect(authorAvatarJid(byArchive.get("mine")!, SELF_FULL)).toBe("rawkode@example.com");
    expect(authorAvatarJid(byArchive.get("theirs")!, SELF_FULL)).toBe("bob@example.com");
  });

  test("a MUC-PM hit uses the occupant mapping in effect when it was sent, not the nick's current holder", async () => {
    // Record Alice behind "sam", then (later on the local clock) Bob.
    occupantJidDirectory.record(ROOM, "sam", "alice@example.com");
    await Bun.sleep(5);
    const sentByAlice = new Date().toISOString();
    await Bun.sleep(5);
    occupantJidDirectory.record(ROOM, "sam", "bob@example.com");

    const xmpp: MamWasmClient = {
      search_dm_history: async () => page([
        archived({ mam_id: "pm", from: `${ROOM}/sam`, to: SELF, timestamp: sentByAlice }),
      ]),
    };
    const [hit] = await pager(xmpp).searchDmMessages(`${ROOM}/sam`, "matching");

    expect(hit?.authorOccupantJid).toBe(`${ROOM}/sam`);
    expect(authorAvatarJid(hit!, SELF_FULL)).toBe("alice@example.com");
  });

  test("room hits carry the archive real JID, which wins over a reused nick", async () => {
    occupantJidDirectory.record(ROOM, "sam", "bob@example.com");
    const xmpp: MamWasmClient = {
      search_room_history: async () => page([
        archived({
          mam_id: "room-hit",
          message_type: "groupchat",
          is_muc: true,
          from: `${ROOM}/sam`,
          to: SELF,
          author_real_jid: "carol@example.com/laptop",
        }),
        archived({ mam_id: "anon-hit", message_type: "groupchat", is_muc: true, from: `${ROOM}/ghost`, to: SELF }),
      ]),
    };
    const results = await pager(xmpp).searchMessages("general", "matching");
    const byArchive = new Map(results.map((result) => [result.archiveId, result]));

    expect(authorAvatarJid(byArchive.get("room-hit")!, SELF_FULL)).toBe("carol@example.com");
    // No real JID and no mapping when it was sent: initials, never a guess.
    expect(authorAvatarJid(byArchive.get("anon-hit")!, SELF_FULL)).toBeNull();
  });
});

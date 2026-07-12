import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { BrowserXmppClient } from "../src/lib/xmpp/client";
import { useDmReadMarkers } from "../src/dms/read-markers";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { nullResumePersistence, type ResumePersistence } from "../src/lib/xmpp/resume-persistence";
import type { WaddleSession } from "../src/lib/server-auth";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    session_id: "s1",
    user_id: "u1",
    username: "alice",
    avatar_url: null,
    xmpp_localpart: "alice",
    jid: "alice@example.com",
    xmpp_websocket_url: "wss://example.com/xmpp",
    is_expired: false,
    expires_at: null,
    ...partial,
  };
}

function connectedClient(xmpp: Record<string, unknown>): BrowserXmppClient {
  const client = new BrowserXmppClient(session());
  (client as unknown as { xmpp: unknown }).xmpp = xmpp;
  (client as unknown as { connected: boolean }).connected = true;
  return client;
}

function persistedOccupants(...occupants: string[]): ResumePersistence {
  return {
    ...nullResumePersistence,
    loadCatchup: () => ({
      dmLastSeen: occupants.map((occupant, index) => [occupant, {
        timestamp: `2026-07-01T10:00:0${index}.000Z`,
        scope: "muc-occupant" as const,
      }]),
      roomLastSeen: [],
    }),
  };
}

describe("BrowserXmppClient MDS publish preflight", () => {
  test("persisted custom-service occupant scope drives public classification and outbound item id", async () => {
    const alice = "room@rooms.custom.example/alice";
    const publish = mock(async () => undefined);
    const client = new BrowserXmppClient(session(), persistedOccupants(alice));
    (client as unknown as { xmpp: unknown }).xmpp = {
      publish_mds_displayed: publish,
      supports_mds_publish_options: mock(async () => true),
    };
    (client as unknown as { connected: boolean }).connected = true;

    expect(client.isMucPmPeer(alice)).toBe(true);
    expect(client.isMucPmPeer("bob@example.com/phone")).toBe(false);

    const markers = useDmReadMarkers({
      xmppClient: ref(client),
      activePeerJid: ref(alice),
      messages: ref<TimelineMessage[]>([{
        id: "pm-1",
        stanzaId: "pm-sid-1",
        stanzaIdBy: "example.com",
        body: "",
        nick: "alice",
        timestamp: 0,
      } as TimelineMessage]),
    });
    markers.markDisplayed("pm-1");
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(publish).toHaveBeenCalledWith(alice, "pm-sid-1", "example.com");
  });

  test("persisted occupant identities survive MDS catch-up and live events", async () => {
    const alice = "room@rooms.custom.example/alice";
    const bob = "room@rooms.custom.example/bob";
    let onDisplayed: ((entry: {
      chat_id: string;
      stanza_id: string;
      stanza_id_by: string;
    }) => void) | undefined;
    const xmpp = {
      fetch_mds_displayed: mock(async () => [
        { chat_id: alice, stanza_id: "alice-catchup", stanza_id_by: "archive.example" },
        { chat_id: bob, stanza_id: "bob-catchup", stanza_id_by: "archive.example" },
      ]),
      subscribe_mds_displayed: mock(async () => undefined),
      set_on_mds_displayed: (handler: typeof onDisplayed) => { onDisplayed = handler; },
    };
    const client = connectedClient(xmpp);
    const received: Array<{ chatId: string; stanzaId: string }> = [];
    client.setMdsDisplayedHandler((entry) => received.push(entry));

    (client as unknown as { wireEvents: (handle: typeof xmpp) => void }).wireEvents(xmpp);
    await (client as unknown as { bootstrapMdsDisplayed: (handle: typeof xmpp) => Promise<void> })
      .bootstrapMdsDisplayed(xmpp);
    onDisplayed?.({ chat_id: alice, stanza_id: "alice-live", stanza_id_by: "archive.example" });

    expect(received).toEqual([
      { chatId: alice, stanzaId: "alice-catchup", stanzaIdBy: "archive.example" },
      { chatId: bob, stanzaId: "bob-catchup", stanzaIdBy: "archive.example" },
      { chatId: alice, stanzaId: "alice-live", stanzaIdBy: "archive.example" },
    ]);
  });

  test("a discovered custom MUC service scopes later Browser MAM paging to the occupant", async () => {
    const alice = "room@rooms.custom.example/alice";
    const fetchPage = mock(async (peerJid: string) => ({
      messages: [],
      first_archive_id: undefined,
      last_archive_id: undefined,
      complete: true,
      peerJid,
    }));
    const client = connectedClient({ fetch_dm_history_page: fetchPage });

    // `discoverTopology()` assigns this field from the disco result. Keep this
    // test focused on the Browser-to-MAM handoff rather than duplicating the
    // exhaustive XEP-0030 fixtures in discovery-pin-permission.test.ts.
    (client as unknown as { mucServiceJid: string }).mucServiceJid = "rooms.custom.example";
    await client.queryPersonalMamPage(alice);

    expect(client.isMucPmPeer(alice)).toBe(true);
    expect(fetchPage.mock.calls[0]?.[0]).toBe(alice);
  });

  test("does not publish when the server lacks pubsub publish-options", async () => {
    const publish = mock(async () => undefined);
    const supports = mock(async () => false);
    const client = connectedClient({
      publish_mds_displayed: publish,
      supports_mds_publish_options: supports,
    });

    await client.publishMdsDisplayed("bob@example.com", "sid-1", "example.com");

    expect(supports).toHaveBeenCalledTimes(1);
    expect(publish).not.toHaveBeenCalled();
  });

  test("publishes when publish-options are advertised and caches the preflight", async () => {
    const publish = mock(async () => undefined);
    const supports = mock(async () => true);
    const client = connectedClient({
      publish_mds_displayed: publish,
      supports_mds_publish_options: supports,
    });

    await client.publishMdsDisplayed("bob@example.com", "sid-1", "example.com");
    await client.publishMdsDisplayed("bob@example.com", "sid-2", "example.com");

    expect(supports).toHaveBeenCalledTimes(1);
    expect(publish).toHaveBeenCalledTimes(2);
  });
});

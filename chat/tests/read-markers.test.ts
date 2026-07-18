// Direct unit tests for useChannelReadMarkers and useDmReadMarkers
// covering XEP-0333 displayed-marker outbound and the
// latestRemoteMessageId computed.

import { afterEach, describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelReadMarkers } from "../src/channels/read-markers";
import { useDmReadMarkers } from "../src/dms/read-markers";
import { useReadReceiptPreference } from "../src/preferences/read-receipts";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { ChannelSummary } from "../src/lib/chat-types";
import { __setFaroForTesting } from "../src/lib/telemetry";

const NS_STANZA_ID = "urn:xmpp:sid:0";

function makeChannelClient(
  sendDisplayed = mock(async () => undefined),
  publishMdsDisplayed = mock(async () => undefined),
): BrowserXmppClient {
  return { sendDisplayed, publishMdsDisplayed } as unknown as BrowserXmppClient;
}
function makeDmClient(
  sendDmDisplayed = mock(async () => undefined),
  publishMdsDisplayed = mock(async () => undefined),
  isMucPmPeer = (_peerJid: string) => false,
): BrowserXmppClient {
  return { sendDmDisplayed, publishMdsDisplayed, isMucPmPeer } as unknown as BrowserXmppClient;
}
function channel(features: string[] = [NS_STANZA_ID]): ChannelSummary {
  return {
    id: "general",
    name: "general",
    jid: "general@rooms.example.com",
    features,
  };
}

afterEach(() => {
  useReadReceiptPreference().setReadReceiptPreference("send");
  __setFaroForTesting(null);
});

function faroEvents() {
  const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
  __setFaroForTesting({
    api: {
      pushEvent: (name: string, attributes?: Record<string, string>) => events.push({ name, attributes }),
    },
  } as never);
  return events;
}

describe("useChannelReadMarkers", () => {
  test("markDisplayed does not invent a MUC marker target without a room stanza-id", () => {
    const sendDisplayed = mock(async () => undefined);
    const publishMdsDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      { id: "m1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed, publishMdsDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      currentChannel: ref(channel()),
      messages,
    });
    r.markDisplayed("m1");
    expect(sendDisplayed).not.toHaveBeenCalled();
    expect(publishMdsDisplayed).not.toHaveBeenCalled();
  });

  test("markDisplayed uses the room stanza-id when XEP-0359 stamped the MUC message", () => {
    const sendDisplayed = mock(async () => undefined);
    const publishMdsDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "sender-id",
        stanzaId: "room-stanza-id",
        stanzaIdBy: "general@rooms.example.com",
        body: "",
        nick: "",
        timestamp: 0,
      } as TimelineMessage,
    ]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed, publishMdsDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      currentChannel: ref(channel()),
      messages,
    });

    r.markDisplayed("sender-id");

    expect(sendDisplayed).toHaveBeenCalledTimes(1);
    expect(sendDisplayed.mock.calls[0]![2]).toBe("room-stanza-id");
    expect(publishMdsDisplayed).toHaveBeenCalledWith(
      "general@rooms.example.com",
      "room-stanza-id",
      "general@rooms.example.com",
    );
  });

  test("reports a failed room displayed-marker send without message identifiers", async () => {
    const events = faroEvents();
    const sendDisplayed = mock(async () => { throw new Error("write failed"); });
    const messages = ref<TimelineMessage[]>([{
      id: "private-message-id",
      stanzaId: "private-stanza-id",
      stanzaIdBy: "general@rooms.example.com",
      createdAt: "2020-01-01T00:00:00Z",
      body: "private body",
      nick: "bob",
    } as TimelineMessage]);
    const markers = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      currentChannel: ref(channel()),
      messages,
    });

    markers.markDisplayed("private-message-id");
    await Promise.resolve();

    expect(events).toEqual([{
      name: "chat.xmpp.displayed_marker.failed",
      attributes: {
        direction: "send",
        kind: "room",
        reason: "send-failed",
        round_trip_latency_band: "over-5s",
      },
    }]);
  });

  test("read-receipt opt-out suppresses MUC XEP-0333 while preserving MDS", () => {
    useReadReceiptPreference().setReadReceiptPreference("suppress");
    const sendDisplayed = mock(async () => undefined);
    const publishMdsDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "sender-id",
        stanzaId: "room-stanza-id",
        stanzaIdBy: "general@rooms.example.com",
        body: "",
        nick: "",
        timestamp: 0,
      } as TimelineMessage,
    ]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed, publishMdsDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      currentChannel: ref(channel()),
      messages,
    });

    r.markDisplayed("sender-id");

    expect(sendDisplayed).not.toHaveBeenCalled();
    expect(publishMdsDisplayed).toHaveBeenCalledWith(
      "general@rooms.example.com",
      "room-stanza-id",
      "general@rooms.example.com",
    );
  });

  test("markDisplayed can suppress MDS for thread-panel displayed markers", () => {
    const sendDisplayed = mock(async () => undefined);
    const publishMdsDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "sender-id",
        stanzaId: "room-stanza-id",
        stanzaIdBy: "general@rooms.example.com",
        body: "",
        nick: "",
        timestamp: 0,
        threadId: "thread-1",
      } as TimelineMessage,
    ]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed, publishMdsDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      currentChannel: ref(channel()),
      messages,
    });

    r.markDisplayed("sender-id", { syncMds: false });

    expect(sendDisplayed).toHaveBeenCalledTimes(1);
    expect(sendDisplayed.mock.calls[0]![3]).toEqual({ id: "thread-1" });
    expect(publishMdsDisplayed).not.toHaveBeenCalled();
  });

  test("markDisplayed is a no-op when target is not in timeline", () => {
    const sendDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      currentChannel: ref(channel()),
      messages,
    });
    r.markDisplayed("not-in-timeline");
    expect(sendDisplayed).not.toHaveBeenCalled();
  });

  test("markDisplayed is a no-op when the room has not advertised XEP-0359 stanza IDs", () => {
    const sendDisplayed = mock(async () => undefined);
    const publishMdsDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "sender-id",
        stanzaId: "room-stanza-id",
        stanzaIdBy: "general@rooms.example.com",
        body: "",
        nick: "",
        timestamp: 0,
      } as TimelineMessage,
    ]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed, publishMdsDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      currentChannel: ref(channel([])),
      messages,
    });
    r.markDisplayed("sender-id");
    expect(sendDisplayed).not.toHaveBeenCalled();
    expect(publishMdsDisplayed).not.toHaveBeenCalled();
  });

  test("markDisplayed is a no-op without a channel", () => {
    const sendDisplayed = mock(async () => undefined);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref<string | null>(null),
      currentChannel: ref(channel()),
      messages: ref<TimelineMessage[]>([]),
    });
    r.markDisplayed("anything");
    expect(sendDisplayed).not.toHaveBeenCalled();
  });

  test("latestRemoteMessageId tracks the last non-self message", () => {
    const messages = ref<TimelineMessage[]>([
      { id: "a", isSelf: false, body: "", nick: "", timestamp: 0 } as TimelineMessage,
      { id: "b", isSelf: true, body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient()),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      currentChannel: ref(channel()),
      messages,
    });
    expect(r.latestRemoteMessageId.value).toBe("a");
  });
});

describe("useDmReadMarkers", () => {
  test("markDisplayed forwards markable DMs to sendDmDisplayed", () => {
    const sendDmDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "dm1",
        body: "",
        nick: "",
        timestamp: 0,
        displayedMarkerRequested: true,
      } as TimelineMessage,
    ]);
    const r = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmDisplayed)),
      activePeerJid: ref("bob@example.com"),
      messages,
    });
    r.markDisplayed("dm1");
    expect(sendDmDisplayed).toHaveBeenCalledTimes(1);
    expect(sendDmDisplayed.mock.calls[0]![0]).toBe("bob@example.com");
    expect(sendDmDisplayed.mock.calls[0]![1]).toBe("dm1");
  });

  test("reports a failed DM displayed-marker send with a latency band", async () => {
    const events = faroEvents();
    const sendDmDisplayed = mock(async () => { throw new Error("write failed"); });
    const markers = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmDisplayed)),
      activePeerJid: ref("bob@example.com"),
      messages: ref<TimelineMessage[]>([{
        id: "dm-private-id",
        createdAt: "2020-01-01T00:00:00Z",
        body: "private body",
        nick: "bob",
        displayedMarkerRequested: true,
      } as TimelineMessage]),
    });

    markers.markDisplayed("dm-private-id");
    await Promise.resolve();

    expect(events[0]).toEqual({
      name: "chat.xmpp.displayed_marker.failed",
      attributes: {
        direction: "send",
        kind: "dm",
        reason: "send-failed",
        round_trip_latency_band: "over-5s",
      },
    });
  });

  test("markDisplayed echoes XEP-0201 thread metadata for threaded DMs", () => {
    const sendDmDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "dm1",
        body: "",
        nick: "",
        timestamp: 0,
        threadId: "dm-child-thread",
        parentThreadId: "dm-root-thread",
        displayedMarkerRequested: true,
      } as TimelineMessage,
    ]);
    const r = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmDisplayed)),
      activePeerJid: ref("bob@example.com"),
      messages,
    });
    r.markDisplayed("dm1");
    expect(sendDmDisplayed).toHaveBeenCalledTimes(1);
    expect(sendDmDisplayed.mock.calls[0]![2]).toEqual({ id: "dm-child-thread", parent: "dm-root-thread" });
  });

  test("markDisplayed does not send opportunistic 1:1 XEP-0333 markers", () => {
    const sendDmDisplayed = mock(async () => undefined);
    const publishMdsDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "dm1",
        stanzaId: "server-stanza-id",
        stanzaIdBy: "example.com",
        body: "",
        nick: "",
        timestamp: 0,
      } as TimelineMessage,
    ]);
    const r = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmDisplayed, publishMdsDisplayed)),
      activePeerJid: ref("bob@example.com"),
      messages,
    });

    r.markDisplayed("dm1");

    expect(sendDmDisplayed).not.toHaveBeenCalled();
    expect(publishMdsDisplayed).toHaveBeenCalledWith(
      "bob@example.com",
      "server-stanza-id",
      "example.com",
    );
  });

  test("read-receipt opt-out suppresses DM XEP-0333 while preserving MDS", () => {
    useReadReceiptPreference().setReadReceiptPreference("suppress");
    const sendDmDisplayed = mock(async () => undefined);
    const publishMdsDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "dm1",
        stanzaId: "server-stanza-id",
        stanzaIdBy: "example.com",
        body: "",
        nick: "",
        timestamp: 0,
        displayedMarkerRequested: true,
      } as TimelineMessage,
    ]);
    const r = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmDisplayed, publishMdsDisplayed)),
      activePeerJid: ref("bob@example.com"),
      messages,
    });

    r.markDisplayed("dm1");

    expect(sendDmDisplayed).not.toHaveBeenCalled();
    expect(publishMdsDisplayed).toHaveBeenCalledWith(
      "bob@example.com",
      "server-stanza-id",
      "example.com",
    );
  });

  test("MUC-PM MDS uses the full occupant while ordinary resources stay bare", () => {
    const occupantPublish = mock(async () => undefined);
    const occupant = "room@conference.example/alice";
    const occupantMarkers = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(
        mock(async () => undefined),
        occupantPublish,
        (peerJid) => peerJid.startsWith("room@conference.example/"),
      )),
      activePeerJid: ref(occupant),
      messages: ref<TimelineMessage[]>([{
        id: "pm-1",
        stanzaId: "pm-sid-1",
        stanzaIdBy: "example.com",
        body: "",
        nick: "alice",
        timestamp: 0,
      } as TimelineMessage]),
    });

    occupantMarkers.markDisplayed("pm-1");

    expect(occupantPublish).toHaveBeenCalledWith(occupant, "pm-sid-1", "example.com");

    const dmPublish = mock(async () => undefined);
    const dmMarkers = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(mock(async () => undefined), dmPublish)),
      activePeerJid: ref("bob@example.com/phone"),
      messages: ref<TimelineMessage[]>([{
        id: "dm-resource-1",
        stanzaId: "dm-sid-1",
        stanzaIdBy: "example.com",
        body: "",
        nick: "bob",
        timestamp: 0,
      } as TimelineMessage]),
    });

    dmMarkers.markDisplayed("dm-resource-1");

    expect(dmPublish).toHaveBeenCalledWith("bob@example.com", "dm-sid-1", "example.com");
  });

  test("restored MUC-PM scope publishes the full item id before client discovery", () => {
    const publishMdsDisplayed = mock(async () => undefined);
    const occupant = "room@rooms.custom.example/alice";
    const markers = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(
        mock(async () => undefined),
        publishMdsDisplayed,
        () => false,
      )),
      activePeerJid: ref(occupant),
      conversationScope: () => "muc-occupant",
      messages: ref<TimelineMessage[]>([{
        id: "pm-restored",
        stanzaId: "pm-restored-sid",
        stanzaIdBy: "example.com",
        body: "",
        nick: "alice",
        timestamp: 0,
      } as TimelineMessage]),
    });

    markers.markDisplayed("pm-restored");

    expect(publishMdsDisplayed).toHaveBeenCalledWith(
      occupant,
      "pm-restored-sid",
      "example.com",
    );
  });

  test("markDisplayed can suppress DM MDS for thread-panel displayed markers", () => {
    const sendDmDisplayed = mock(async () => undefined);
    const publishMdsDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      {
        id: "dm1",
        stanzaId: "server-stanza-id",
        stanzaIdBy: "example.com",
        body: "",
        nick: "",
        timestamp: 0,
        threadId: "dm-thread",
        displayedMarkerRequested: true,
      } as TimelineMessage,
    ]);
    const r = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmDisplayed, publishMdsDisplayed)),
      activePeerJid: ref("bob@example.com"),
      messages,
    });

    r.markDisplayed("dm1", { syncMds: false });

    expect(sendDmDisplayed).toHaveBeenCalledTimes(1);
    expect(sendDmDisplayed.mock.calls[0]![2]).toEqual({ id: "dm-thread" });
    expect(publishMdsDisplayed).not.toHaveBeenCalled();
  });

  test("no-op without active peer", () => {
    const sendDmDisplayed = mock(async () => undefined);
    const r = useDmReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmDisplayed)),
      activePeerJid: ref<string | null>(null),
      messages: ref<TimelineMessage[]>([]),
    });
    r.markDisplayed("anything");
    expect(sendDmDisplayed).not.toHaveBeenCalled();
  });
});

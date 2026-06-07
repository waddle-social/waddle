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
): BrowserXmppClient {
  return { sendDmDisplayed, publishMdsDisplayed } as unknown as BrowserXmppClient;
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
});

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

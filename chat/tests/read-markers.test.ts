// Direct unit tests for useChannelReadMarkers and useDmReadMarkers
// covering XEP-0333 displayed-marker outbound and the
// latestRemoteMessageId computed.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelReadMarkers } from "../src/channels/read-markers";
import { useDmReadMarkers } from "../src/dms/read-markers";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { TimelineMessage } from "../src/lib/chat-ui";

function makeChannelClient(sendDisplayed = mock(async () => undefined)): BrowserXmppClient {
  return { sendDisplayed } as unknown as BrowserXmppClient;
}
function makeDmClient(sendDmDisplayed = mock(async () => undefined)): BrowserXmppClient {
  return { sendDmDisplayed } as unknown as BrowserXmppClient;
}

describe("useChannelReadMarkers", () => {
  test("markDisplayed forwards the message's id to sendDisplayed", () => {
    const sendDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      { id: "m1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      messages,
    });
    r.markDisplayed("m1");
    expect(sendDisplayed).toHaveBeenCalledTimes(1);
    expect(sendDisplayed.mock.calls[0]![2]).toBe("m1");
  });

  test("markDisplayed falls back to the passed messageId when target not in timeline", () => {
    const sendDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([]);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
      messages,
    });
    r.markDisplayed("not-in-timeline");
    expect(sendDisplayed.mock.calls[0]![2]).toBe("not-in-timeline");
  });

  test("markDisplayed is a no-op without a channel", () => {
    const sendDisplayed = mock(async () => undefined);
    const r = useChannelReadMarkers({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendDisplayed)),
      activeSpaceId: ref("space"),
      activeChannelId: ref<string | null>(null),
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
      messages,
    });
    expect(r.latestRemoteMessageId.value).toBe("a");
  });
});

describe("useDmReadMarkers", () => {
  test("markDisplayed forwards to sendDmDisplayed", () => {
    const sendDmDisplayed = mock(async () => undefined);
    const messages = ref<TimelineMessage[]>([
      { id: "dm1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
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

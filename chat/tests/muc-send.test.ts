// Direct unit tests for useMucSend (PR Compliance #2 — non-trivial extracted
// behavior gets direct tests at the composable level, not only via integration).
// No Vue component mounting; the composable accepts plain refs and callbacks
// so we drive it from regular bun tests.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useMucSend } from "../src/channels/muc-send";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { ChannelSummary } from "../src/lib/chat-types";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";

const session: WaddleSession = {
  username: "alice",
  jid: "alice@example.com/desktop",
  session_id: "tok",
  xmpp_websocket_url: "wss://example.com/ws",
};

const channel: ChannelSummary = { id: "general", name: "General" };

function makeClient(overrides: Partial<BrowserXmppClient> = {}): BrowserXmppClient {
  return {
    sendGroupMessage: mock(async () => ({ id: "sid-1", state: "sending" })),
    sendChatState: mock(async () => undefined),
    sendCorrection: mock(async () => undefined),
    uploadAttachments: mock(async () => []),
    ...overrides,
  } as unknown as BrowserXmppClient;
}

function harness(opts: {
  client?: BrowserXmppClient;
  channelId?: string | null;
  draft?: string;
  currentChannel?: ChannelSummary;
} = {}) {
  const xmppClient = ref<BrowserXmppClient | null>(opts.client ?? makeClient());
  const activeChannelId = ref<string | null>(opts.channelId === undefined ? "general" : opts.channelId);
  const draft = ref(opts.draft ?? "");
  const messages = ref<TimelineMessage[]>([]);
  const actionError = ref("");
  const clearActionError = mock(() => { actionError.value = ""; });
  const scrollToPinnedEdgeAndPin = mock(async () => true);
  const onSendComplete = mock(() => {});

  const send = useMucSend({
    session: ref(session),
    xmppClient,
    activeSpaceId: ref("space"),
    activeChannelId,
    currentChannel: ref(opts.currentChannel ?? channel),
    currentRoomJid: ref("general@muc.example.com"),
    messages,
    draft,
    forumPostTitle: ref(""),
    actionError,
    clearActionError,
    normalizeError: (e) => String(e),
    scrollToPinnedEdgeAndPin,
    onSendComplete,
  });

  return { xmppClient, activeChannelId, draft, messages, actionError, send, onSendComplete, scrollToPinnedEdgeAndPin };
}

describe("useMucSend.sendMessage — happy path", () => {
  test("optimistic-inserts a self message, registers pending echo, clears draft, fires onSendComplete", async () => {
    const h = harness({ draft: "hello world" });

    await h.send.sendMessage(undefined, []);

    // Optimistic insert lands with isSelf and "sending" delivery
    expect(h.messages.value.length).toBe(1);
    expect(h.messages.value[0]?.isSelf).toBe(true);
    expect(h.messages.value[0]?.id).toBe("sid-1");
    expect(h.messages.value[0]?.body).toBe("hello world");
    expect(h.messages.value[0]?.deliveryStatus).toBe("sending");

    // Pending echo set tracks the client-assigned id for self-echo reconciliation
    expect(h.send.pendingEchoClientIds.has("sid-1")).toBe(true);

    // Draft cleared (composer send: markup was passed as [])
    expect(h.draft.value).toBe("");

    // isSending false after finally, onSendComplete invoked once
    expect(h.send.isSending.value).toBe(false);
    expect(h.onSendComplete).toHaveBeenCalledTimes(1);
  });
});

describe("useMucSend.sendMessage — guards", () => {
  test("empty body + no files + no metadata: silent no-op (no send, no insert)", async () => {
    const h = harness({ draft: "   " });
    await h.send.sendMessage();
    expect(h.messages.value.length).toBe(0);
    expect(h.send.isSending.value).toBe(false);
    expect((h.xmppClient.value!.sendGroupMessage as unknown as ReturnType<typeof mock>).mock.calls.length).toBe(0);
  });

  test("no client: silent no-op", async () => {
    const h = harness({ client: undefined });
    h.xmppClient.value = null;
    await h.send.sendMessage("hello", []);
    expect(h.messages.value.length).toBe(0);
    expect(h.actionError.value).toBe("");
  });

  test("file too large: actionError set, no send", async () => {
    const h = harness();
    // Construct an oversized "file" (Blob with .size > MAX_FILE_UPLOAD_BYTES)
    const big = { size: 999_999_999 } as File;
    await h.send.sendMessage("hi", [], undefined, [big]);
    expect(h.actionError.value).toContain("File too large");
    expect(h.messages.value.length).toBe(0);
    expect((h.xmppClient.value!.sendGroupMessage as unknown as ReturnType<typeof mock>).mock.calls.length).toBe(0);
  });

  test("forum post without title: actionError set, no send", async () => {
    const h = harness({
      draft: "body",
      currentChannel: { id: "general", name: "General", channel_type: "forum" } as ChannelSummary,
    });
    await h.send.sendMessage(undefined, []);
    expect(h.actionError.value).toBe("Add a title before posting to this forum.");
    expect(h.messages.value.length).toBe(0);
  });
});

describe("useMucSend.sendMessage — reply guard (XEP-0461 §3.2)", () => {
  test("replyTo to an unreplyable target: actionError set, isSending reset, no optimistic insert", async () => {
    const h = harness({ draft: "reply text" });
    // Seed the timeline with a parent that lacks replyableId (no XEP-0359
    // stanza-id from the room) — sending should be refused per XEP-0461 §3.2.
    h.messages.value = [
      {
        id: "parent-1",
        body: "older message",
        nick: "bob",
        timestamp: 0,
        isSelf: false,
        // intentionally NO replyableId
      } as TimelineMessage,
    ];

    await h.send.sendMessage(undefined, [], undefined, undefined, {
      id: "parent-1",
      author: "bob",
    });

    expect(h.actionError.value).toContain("no room stanza-id");
    expect(h.send.isSending.value).toBe(false);
    // The original parent is still there; no new optimistic message.
    expect(h.messages.value.length).toBe(1);
  });
});

describe("useMucSend.sendMessage — failure paths", () => {
  test("send throws: actionError = normalized, isSending reset, no optimistic insert", async () => {
    const client = makeClient({
      sendGroupMessage: mock(async () => { throw new Error("server angry"); }),
    } as Partial<BrowserXmppClient>);
    const h = harness({ client, draft: "hello" });

    await h.send.sendMessage(undefined, []);

    expect(h.actionError.value).toContain("server angry");
    expect(h.send.isSending.value).toBe(false);
    expect(h.messages.value.length).toBe(0);
  });

  test("isStillCurrentChannel false after await (channel swap): no optimistic insert", async () => {
    let resolveSend: (v: { id: string; state: "queued" | "sending" }) => void = () => {};
    const client = makeClient({
      sendGroupMessage: mock(() => new Promise<{ id: string; state: "queued" | "sending" }>((resolve) => {
        resolveSend = resolve;
      })),
    } as Partial<BrowserXmppClient>);
    const h = harness({ client, draft: "hello" });

    const sendPromise = h.send.sendMessage(undefined, []);
    // Simulate channel swap mid-send
    h.activeChannelId.value = "different-channel";
    resolveSend({ id: "sid-late", state: "sending" });
    await sendPromise;

    // No optimistic insert because the channel changed before the await resolved
    expect(h.messages.value.length).toBe(0);
    expect(h.send.isSending.value).toBe(false);
  });
});

describe("useMucSend.sendMessage — thread routing (GIF picks)", () => {
  // Regression: GIFs picked from inside a thread used to land in the parent
  // channel because sendGif never forwarded a threadOverride. The composer's
  // text path always built one in ThreadPanel.onSend; the GIF emit chain now
  // mirrors it, so sendMessage gets threadOverride.threadId in arg 6.
  test("threadOverride.threadId is forwarded to client.sendGroupMessage as opts.threadId", async () => {
    const sendGroupMessage = mock(async () => ({ id: "sid-gif", state: "sending" }));
    const client = makeClient({ sendGroupMessage } as Partial<BrowserXmppClient>);
    const h = harness({ client });

    await h.send.sendMessage(
      "https://giphy.example/animated.gif",
      [],
      [],
      undefined,
      undefined,
      { threadId: "thread-42" },
    );

    expect(sendGroupMessage).toHaveBeenCalledTimes(1);
    const call = (sendGroupMessage as unknown as ReturnType<typeof mock>).mock.calls[0]!;
    // sendGroupMessage(spaceId, channelId, body, opts) — threadId on opts (arg 4)
    expect(call[3].threadId).toBe("thread-42");
    expect(call[2]).toBe("https://giphy.example/animated.gif");
  });

  test("threadOverride.parentThreadId is forwarded for nested sub-threads", async () => {
    const sendGroupMessage = mock(async () => ({ id: "sid-gif", state: "sending" }));
    const client = makeClient({ sendGroupMessage } as Partial<BrowserXmppClient>);
    const h = harness({ client });

    await h.send.sendMessage(
      "https://giphy.example/nested.gif",
      [],
      [],
      undefined,
      undefined,
      { threadId: "thread-child", parentThreadId: "thread-parent" },
    );

    const call = (sendGroupMessage as unknown as ReturnType<typeof mock>).mock.calls[0]!;
    expect(call[3].threadId).toBe("thread-child");
    expect(call[3].parentThreadId).toBe("thread-parent");
  });
});

describe("useMucSend.editMessage", () => {
  test("delegates to client.sendCorrection with the message's correctionTargetId", async () => {
    const sendCorrection = mock(async () => undefined);
    const client = makeClient({ sendCorrection } as Partial<BrowserXmppClient>);
    const h = harness({ client });
    h.messages.value = [
      {
        id: "msg-1",
        correctionTargetId: "original-server-id",
        body: "old text",
        nick: "alice",
        timestamp: 0,
        isSelf: true,
      } as TimelineMessage,
    ];

    await h.send.editMessage("msg-1", "new text");

    expect(sendCorrection).toHaveBeenCalledTimes(1);
    const call = (sendCorrection as unknown as ReturnType<typeof mock>).mock.calls[0]!;
    // Target id is correctionTargetId, not the message id
    expect(call[3]).toBe("original-server-id");
  });

  test("empty body: no send, no error", async () => {
    const sendCorrection = mock(async () => undefined);
    const client = makeClient({ sendCorrection } as Partial<BrowserXmppClient>);
    const h = harness({ client });
    await h.send.editMessage("any-id", "   ");
    expect(sendCorrection).not.toHaveBeenCalled();
    expect(h.actionError.value).toBe("");
  });
});

describe("useMucSend delivery lifecycle handlers", () => {
  test("onMessageAck promotes a sending self-message to delivered", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", isSelf: true, deliveryStatus: "sending", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    h.send.onMessageAck("m1");
    expect(h.messages.value[0]?.deliveryStatus).toBe("delivered");
  });

  test("onMessageQueueStatus + onMessageDeliveryFailure each apply via the pure helper", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", isSelf: true, deliveryStatus: "queued", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];

    h.send.onMessageQueueStatus("m1", "sending");
    expect(h.messages.value[0]?.deliveryStatus).toBe("sending");

    h.send.onMessageDeliveryFailure("m1");
    expect(h.messages.value[0]?.deliveryStatus).toBe("failed");
  });

  test("onMessageAck after delivered stays delivered (idempotent terminal)", () => {
    // Reference-identity preservation is verified at the pure-helper level in
    // tests/timeline-state.test.ts; here we just check the observable
    // outcome (status unchanged) since the Vue reactive proxy on the ref's
    // value array breaks raw === identity comparisons.
    const h = harness();
    h.messages.value = [
      { id: "m1", isSelf: true, deliveryStatus: "delivered", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    h.send.onMessageAck("m1");
    expect(h.messages.value[0]?.deliveryStatus).toBe("delivered");
    expect(h.messages.value.length).toBe(1);
  });
});

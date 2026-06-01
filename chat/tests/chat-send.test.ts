// Direct unit tests for useChatSend (PR Compliance #2 — non-trivial extracted
// behavior gets direct tests at the composable level). No Vue component
// mounting; the composable takes plain refs and callbacks.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChatSend } from "../src/dms/chat-send";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";

const session: WaddleSession = {
  username: "alice",
  jid: "alice@example.com/desktop",
  session_id: "tok",
  xmpp_websocket_url: "wss://example.com/ws",
};

function makeClient(overrides: Partial<BrowserXmppClient> = {}): BrowserXmppClient {
  return {
    sendDirectMessage: mock(async () => ({ id: "dm-sid-1", state: "sending" })),
    sendDmChatState: mock(async () => undefined),
    sendDmCorrection: mock(async () => undefined),
    uploadAttachments: mock(async () => []),
    ...overrides,
  } as unknown as BrowserXmppClient;
}

function harness(opts: {
  client?: BrowserXmppClient;
  peerJid?: string | null;
  draft?: string;
} = {}) {
  const xmppClient = ref<BrowserXmppClient | null>(opts.client ?? makeClient());
  const activePeerJid = ref<string | null>(opts.peerJid === undefined ? "bob@example.com" : opts.peerJid);
  const draft = ref(opts.draft ?? "");
  const messages = ref<TimelineMessage[]>([]);
  const actionError = ref("");
  const clearActionError = mock(() => { actionError.value = ""; });
  const scrollToPinnedEdgeAndPin = mock(async () => true);
  const onSendComplete = mock(() => {});

  const send = useChatSend({
    session: ref(session),
    xmppClient,
    activePeerJid,
    messages,
    draft,
    actionError,
    clearActionError,
    normalizeError: (e) => String(e),
    scrollToPinnedEdgeAndPin,
    onSendComplete,
  });

  return { xmppClient, activePeerJid, draft, messages, actionError, send, onSendComplete };
}

describe("useChatSend.sendMessage — happy path", () => {
  test("optimistic-inserts self message, registers pending echo, clears draft, fires onSendComplete", async () => {
    const h = harness({ draft: "hi bob" });

    await h.send.sendMessage(undefined, []);

    expect(h.messages.value.length).toBe(1);
    expect(h.messages.value[0]?.isSelf).toBe(true);
    expect(h.messages.value[0]?.id).toBe("dm-sid-1");
    expect(h.messages.value[0]?.body).toBe("hi bob");
    expect(h.messages.value[0]?.deliveryStatus).toBe("sending");

    expect(h.send.pendingEchoClientIds.has("dm-sid-1")).toBe(true);
    expect(h.draft.value).toBe("");
    expect(h.send.isSending.value).toBe(false);
    expect(h.onSendComplete).toHaveBeenCalledTimes(1);
  });

  test("sends immediately without preview metadata while composer lookup is still loading", async () => {
    const sendDirectMessage = mock(async () => ({ id: "dm-link", state: "sending" }));
    const lookupLinkPreview = mock(() => new Promise<never>(() => {}));
    const client = makeClient({ sendDirectMessage, lookupLinkPreview } as Partial<BrowserXmppClient>);
    const h = harness({ client, draft: "read https://example.com/article" });

    const sent = await Promise.race([
      h.send.sendMessage(undefined, []).then(() => true),
      new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 10)),
    ]);

    expect(sent).toBe(true);
    expect(lookupLinkPreview).not.toHaveBeenCalled();
    const call = (sendDirectMessage as unknown as ReturnType<typeof mock>).mock.calls[0]!;
    expect(call[2].linkPreviewToken).toBeUndefined();
    expect(call[2].linkPreviewExpiresAt).toBeUndefined();
    expect(h.messages.value[0]?.linkPreviews).toBeUndefined();
  });

  test("uses an already-ready composer preview token when sending", async () => {
    const sendDirectMessage = mock(async () => ({ id: "dm-link", state: "sending" }));
    const client = makeClient({ sendDirectMessage } as Partial<BrowserXmppClient>);
    const h = harness({ client, draft: "read https://example.com/article" });

    await h.send.sendMessage(undefined, [], undefined, undefined, undefined, {
      token: "preview-token-1",
      expiresAt: "2999-01-01T00:00:00.000Z",
      preview: {
        originalUrl: "https://example.com/article",
        normalizedUrl: "https://example.com/article",
        title: "Example Article",
        description: "Plain text summary",
      },
    });

    const call = (sendDirectMessage as unknown as ReturnType<typeof mock>).mock.calls[0]!;
    expect(call[2].linkPreviewToken).toBe("preview-token-1");
    expect(call[2].linkPreviewExpiresAt).toBe("2999-01-01T00:00:00.000Z");
    expect(h.messages.value[0]?.linkPreviews).toEqual([{
      originalUrl: "https://example.com/article",
      normalizedUrl: "https://example.com/article",
      title: "Example Article",
      description: "Plain text summary",
    }]);
  });

  test("expired composer preview payload sends normally without preview metadata", async () => {
    const sendDirectMessage = mock(async () => ({ id: "dm-link", state: "sending" }));
    const client = makeClient({ sendDirectMessage } as Partial<BrowserXmppClient>);
    const h = harness({ client, draft: "read https://example.com/article" });

    await h.send.sendMessage(undefined, [], undefined, undefined, undefined, {
      token: "expired-token",
      expiresAt: "2000-01-01T00:00:00.000Z",
      preview: {
        originalUrl: "https://example.com/article",
        normalizedUrl: "https://example.com/article",
        title: "Example Article",
      },
    });

    const call = (sendDirectMessage as unknown as ReturnType<typeof mock>).mock.calls[0]!;
    expect(call[2].linkPreviewToken).toBeUndefined();
    expect(call[2].linkPreviewExpiresAt).toBeUndefined();
    expect(h.messages.value[0]?.linkPreviews).toBeUndefined();
  });
});

describe("useChatSend.sendMessage — guards", () => {
  test("empty body + no files: silent no-op", async () => {
    const h = harness({ draft: "   " });
    await h.send.sendMessage();
    expect(h.messages.value.length).toBe(0);
    expect((h.xmppClient.value!.sendDirectMessage as unknown as ReturnType<typeof mock>).mock.calls.length).toBe(0);
  });

  test("no peerJid: silent no-op", async () => {
    const h = harness({ peerJid: null, draft: "hello" });
    await h.send.sendMessage(undefined, []);
    expect(h.messages.value.length).toBe(0);
    expect(h.actionError.value).toBe("");
  });

  test("file too large: actionError set, no send", async () => {
    const h = harness();
    const big = { size: 999_999_999 } as File;
    await h.send.sendMessage("hi", [], undefined, [big]);
    expect(h.actionError.value).toContain("File too large");
    expect(h.messages.value.length).toBe(0);
  });
});

describe("useChatSend.sendMessage — failure paths", () => {
  test("send throws: actionError = normalized, isSending reset", async () => {
    const client = makeClient({
      sendDirectMessage: mock(async () => { throw new Error("offline"); }),
    } as Partial<BrowserXmppClient>);
    const h = harness({ client, draft: "hello" });

    await h.send.sendMessage(undefined, []);

    expect(h.actionError.value).toContain("offline");
    expect(h.send.isSending.value).toBe(false);
    expect(h.messages.value.length).toBe(0);
  });

  test("isStillActive false after peer swap mid-send: no optimistic insert", async () => {
    let resolveSend: (v: { id: string; state: "queued" | "sending" }) => void = () => {};
    const client = makeClient({
      sendDirectMessage: mock(() => new Promise<{ id: string; state: "queued" | "sending" }>((resolve) => {
        resolveSend = resolve;
      })),
    } as Partial<BrowserXmppClient>);
    const h = harness({ client, draft: "hello" });

    const sendPromise = h.send.sendMessage(undefined, []);
    // Simulate peer change mid-send
    h.activePeerJid.value = "carol@example.com";
    resolveSend({ id: "dm-late", state: "sending" });
    await sendPromise;

    expect(h.messages.value.length).toBe(0);
    expect(h.send.isSending.value).toBe(false);
  });
});

describe("useChatSend.editMessage", () => {
  test("delegates to client.sendDmCorrection with the message's correctionTargetId", async () => {
    const sendDmCorrection = mock(async () => undefined);
    const client = makeClient({ sendDmCorrection } as Partial<BrowserXmppClient>);
    const h = harness({ client });
    h.messages.value = [
      {
        id: "dm-msg-1",
        correctionTargetId: "original-server-id",
        body: "old text",
        nick: "alice",
        timestamp: 0,
        isSelf: true,
      } as TimelineMessage,
    ];

    await h.send.editMessage("dm-msg-1", "new text");

    expect(sendDmCorrection).toHaveBeenCalledTimes(1);
    const call = (sendDmCorrection as unknown as ReturnType<typeof mock>).mock.calls[0]!;
    expect(call[2]).toBe("original-server-id");
  });

  test("empty body: no send", async () => {
    const sendDmCorrection = mock(async () => undefined);
    const client = makeClient({ sendDmCorrection } as Partial<BrowserXmppClient>);
    const h = harness({ client });
    await h.send.editMessage("any-id", "   ");
    expect(sendDmCorrection).not.toHaveBeenCalled();
  });
});

describe("useChatSend delivery lifecycle handlers", () => {
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
});

// Direct unit tests for useDmMessageActions: XEP-0444 reactions, XEP-0424
// retractions, and extension-annotation actions on 1:1 chat.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useDmMessageActions } from "../src/dms/message-actions";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { ExtensionAnnotationAction, TimelineMessage } from "../src/lib/chat-ui";

const session: WaddleSession = {
  username: "alice",
  jid: "alice@example.com/desktop",
  session_id: "tok",
  xmpp_websocket_url: "wss://example.com/ws",
};

function makeClient(overrides: Partial<BrowserXmppClient> = {}): BrowserXmppClient {
  return {
    sendDmReaction: mock(async () => undefined),
    sendDmRetraction: mock(async () => undefined),
    invokeExtensionLaunch: mock(async () => ({ ok: true })),
    ...overrides,
  } as unknown as BrowserXmppClient;
}

function harness(opts: { client?: BrowserXmppClient } = {}) {
  const xmppClient = ref<BrowserXmppClient | null>(opts.client ?? makeClient());
  const messages = ref<TimelineMessage[]>([]);
  const actionError = ref("");
  const clearActionError = mock(() => { actionError.value = ""; });
  const applyReaction = mock(() => {});

  const actions = useDmMessageActions({
    session: ref(session),
    xmppClient,
    activePeerJid: ref("bob@example.com"),
    messages,
    actionError,
    clearActionError,
    normalizeError: (e) => String(e),
    applyReaction,
  });

  return { xmppClient, messages, actionError, actions, applyReaction };
}

describe("toggleReaction (XEP-0444)", () => {
  test("adds an emoji and sends the full updated set", async () => {
    const h = harness();
    h.messages.value = [
      { id: "dm-1", reactions: {}, body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.toggleReaction("dm-1", "🎉");

    expect(h.applyReaction).toHaveBeenCalledTimes(1);
    const sendDmReaction = h.xmppClient.value!.sendDmReaction as unknown as ReturnType<typeof mock>;
    expect(sendDmReaction).toHaveBeenCalledTimes(1);
    expect(sendDmReaction.mock.calls[0]![2]).toEqual(["🎉"]);
  });

  test("rolls back optimistic update on send failure (no XEP-0280 carbon for own send)", async () => {
    const client = makeClient({
      sendDmReaction: mock(async () => { throw new Error("network"); }),
    } as Partial<BrowserXmppClient>);
    const h = harness({ client });
    h.messages.value = [
      { id: "dm-1", reactions: {}, body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];

    await h.actions.toggleReaction("dm-1", "🎉");

    expect(h.applyReaction).toHaveBeenCalledTimes(2);
    const rollback = (h.applyReaction as unknown as ReturnType<typeof mock>).mock.calls[1]!;
    expect(rollback[2]).toEqual([]);
    expect(h.actionError.value).toContain("network");
  });
});

describe("toggleReaction target-id precedence", () => {
  test("prefers replyableId over id when both are present", async () => {
    // DM `replyableId` is the XEP-0461 §3.2 reply-target id —
    // `dmMessageFromArchived` sets it to `origin_id ?? message.id`, which
    // is wire-stable across MAM round-trips. `TimelineMessage.id` can fall
    // back to a MAM archive id when origin-id and message.id are absent;
    // that's not a valid reaction target. Lock the
    // `msg.replyableId ?? msg.id` precedence in.
    const h = harness();
    h.messages.value = [
      {
        id: "client-id",
        replyableId: "server-stable-id",
        reactions: {},
        body: "", nick: "", timestamp: 0,
      } as TimelineMessage,
    ];
    await h.actions.toggleReaction("client-id", "👀");
    const sendDmReaction = h.xmppClient.value!.sendDmReaction as unknown as ReturnType<typeof mock>;
    expect(sendDmReaction).toHaveBeenCalledTimes(1);
    expect(sendDmReaction.mock.calls[0]![1]).toBe("server-stable-id");
  });
});

describe("retractMessage (XEP-0424)", () => {
  test("prefers replyableId over id (wire-stable origin-id beats potential MAM-id fallback)", async () => {
    // `dmMessageFromArchived` sets `replyableId = origin_id ?? message.id`
    // — that's the wire-stable retract target. `TimelineMessage.id` can
    // fall back to a MAM archive id, which is NOT a valid XEP-0424 target.
    const h = harness();
    h.messages.value = [
      {
        id: "mam-archive-id",
        replyableId: "wire-origin-id",
        body: "", nick: "", timestamp: 0,
      } as TimelineMessage,
    ];
    await h.actions.retractMessage("mam-archive-id");
    const sendDmRetraction = h.xmppClient.value!.sendDmRetraction as unknown as ReturnType<typeof mock>;
    expect(sendDmRetraction).toHaveBeenCalledTimes(1);
    expect(sendDmRetraction.mock.calls[0]![1]).toBe("wire-origin-id");
  });

  test("falls back to message id when replyableId is absent (DMs don't require room stanza-id)", async () => {
    const h = harness();
    h.messages.value = [
      { id: "dm-1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.retractMessage("dm-1");
    const sendDmRetraction = h.xmppClient.value!.sendDmRetraction as unknown as ReturnType<typeof mock>;
    expect(sendDmRetraction).toHaveBeenCalledTimes(1);
    expect(sendDmRetraction.mock.calls[0]![1]).toBe("dm-1");
  });

  test("send-throws surfaces normalized error", async () => {
    const client = makeClient({
      sendDmRetraction: mock(async () => { throw new Error("forbidden"); }),
    } as Partial<BrowserXmppClient>);
    const h = harness({ client });
    h.messages.value = [
      { id: "dm-1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.retractMessage("dm-1");
    expect(h.actionError.value).toContain("forbidden");
  });
});

describe("invokeExtensionAction", () => {
  test("forwards launch metadata to the wasm client", async () => {
    const invokeExtensionLaunch = mock(async () => ({ ok: true }));
    const client = makeClient({ invokeExtensionLaunch } as Partial<BrowserXmppClient>);
    const h = harness({ client });

    const action: ExtensionAnnotationAction = {
      annotationId: "a1",
      extensionId: "ext",
      label: "Do it",
      launch: { kind: "launch", url: "https://example.com/x" },
    };
    const result = await h.actions.invokeExtensionAction(action);

    expect(invokeExtensionLaunch).toHaveBeenCalledTimes(1);
    expect(result).toEqual({ ok: true });
  });

  test("throws when no client", async () => {
    const h = harness();
    h.xmppClient.value = null;
    const action: ExtensionAnnotationAction = {
      annotationId: "a1",
      extensionId: "ext",
      label: "Do it",
      launch: { kind: "launch", url: "https://example.com/x" },
    };
    await expect(h.actions.invokeExtensionAction(action)).rejects.toThrow("XMPP session is not ready");
  });

  test("throws when launch metadata is missing", async () => {
    const h = harness();
    const action = { annotationId: "a1", extensionId: "ext", label: "Do it" } as ExtensionAnnotationAction;
    await expect(h.actions.invokeExtensionAction(action))
      .rejects.toThrow("missing launch metadata");
  });
});

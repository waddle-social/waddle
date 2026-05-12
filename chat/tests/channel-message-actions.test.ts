// Direct unit tests for useChannelMessageActions covering XEP-0444 reaction
// replace semantics, XEP-0424 retraction id-trust requirement, XEP-0425
// moderator retract, and the extension-annotation action surface.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelMessageActions } from "../src/channels/message-actions";
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
    sendReaction: mock(async () => undefined),
    sendRetraction: mock(async () => undefined),
    sendModeration: mock(async () => undefined),
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

  const actions = useChannelMessageActions({
    session: ref(session),
    xmppClient,
    activeSpaceId: ref("space"),
    activeChannelId: ref("general"),
    currentRoomJid: ref("general@muc.example.com"),
    messages,
    actionError,
    clearActionError,
    normalizeError: (e) => String(e),
    applyReaction,
  });

  return { xmppClient, messages, actionError, actions, applyReaction };
}

describe("toggleReaction (XEP-0444)", () => {
  test("adds an emoji to the local user's reaction set with replace semantics", async () => {
    const h = harness();
    h.messages.value = [
      { id: "msg-1", reactionTargetId: "stanza-1", reactions: {}, body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.toggleReaction("msg-1", "👍");

    // Optimistic local update fired
    expect(h.applyReaction).toHaveBeenCalledTimes(1);
    const applyCall = (h.applyReaction as unknown as ReturnType<typeof mock>).mock.calls[0]!;
    expect(applyCall[0]).toBe("stanza-1");
    expect(applyCall[1]).toBe("alice");
    expect(applyCall[2]).toEqual(["👍"]);

    // sendReaction received the full updated set per XEP-0444 replace semantics
    const sendReaction = h.xmppClient.value!.sendReaction as unknown as ReturnType<typeof mock>;
    expect(sendReaction).toHaveBeenCalledTimes(1);
    const sendArgs = sendReaction.mock.calls[0]!;
    expect(sendArgs[2]).toBe("stanza-1");
    expect(sendArgs[3]).toEqual(["👍"]);
  });

  test("removes an emoji when the local user has already reacted with it", async () => {
    const h = harness();
    h.messages.value = [
      {
        id: "msg-1",
        reactionTargetId: "stanza-1",
        reactions: { "👍": ["alice"], "❤️": ["alice"] },
        body: "", nick: "", timestamp: 0,
      } as TimelineMessage,
    ];
    await h.actions.toggleReaction("msg-1", "👍");

    // The full updated set for alice no longer includes 👍
    const sendReaction = h.xmppClient.value!.sendReaction as unknown as ReturnType<typeof mock>;
    const sentEmojis = sendReaction.mock.calls[0]![3] as string[];
    expect(sentEmojis).toEqual(["❤️"]);
  });

  test("rolls back the optimistic update if the send fails", async () => {
    const client = makeClient({
      sendReaction: mock(async () => { throw new Error("server angry"); }),
    } as Partial<BrowserXmppClient>);
    const h = harness({ client });
    h.messages.value = [
      { id: "msg-1", reactionTargetId: "stanza-1", reactions: {}, body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];

    await h.actions.toggleReaction("msg-1", "👍");

    // Optimistic + rollback = 2 applyReaction calls
    expect(h.applyReaction).toHaveBeenCalledTimes(2);
    const rollback = (h.applyReaction as unknown as ReturnType<typeof mock>).mock.calls[1]!;
    expect(rollback[2]).toEqual([]); // back to no reactions
    expect(h.actionError.value).toContain("server angry");
  });

  test("no-op without reactionTargetId (message can't be reacted to)", async () => {
    const h = harness();
    h.messages.value = [
      // No reactionTargetId — message wasn't reflected by the room with a stanza-id.
      { id: "msg-1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.toggleReaction("msg-1", "👍");
    expect((h.xmppClient.value!.sendReaction as unknown as ReturnType<typeof mock>).mock.calls.length).toBe(0);
    expect(h.applyReaction).not.toHaveBeenCalled();
  });
});

describe("retractMessage (XEP-0424)", () => {
  test("sends with replyableId (room stanza-id)", async () => {
    const h = harness();
    h.messages.value = [
      { id: "msg-1", replyableId: "stanza-by-room", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.retractMessage("msg-1");
    const sendRetraction = h.xmppClient.value!.sendRetraction as unknown as ReturnType<typeof mock>;
    expect(sendRetraction).toHaveBeenCalledTimes(1);
    expect(sendRetraction.mock.calls[0]![2]).toBe("stanza-by-room");
  });

  test("refuses without replyableId — origin-id is too weak to retract by (XEP-0359 §5)", async () => {
    const h = harness();
    h.messages.value = [
      { id: "msg-1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.retractMessage("msg-1");
    const sendRetraction = h.xmppClient.value!.sendRetraction as unknown as ReturnType<typeof mock>;
    expect(sendRetraction).not.toHaveBeenCalled();
    expect(h.actionError.value).toContain("no room stanza-id");
  });

  test("send-throws surfaces normalized error", async () => {
    const client = makeClient({
      sendRetraction: mock(async () => { throw new Error("forbidden"); }),
    } as Partial<BrowserXmppClient>);
    const h = harness({ client });
    h.messages.value = [
      { id: "msg-1", replyableId: "stanza-1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.retractMessage("msg-1");
    expect(h.actionError.value).toContain("forbidden");
  });
});

describe("moderateMessage (XEP-0425, MUC-only)", () => {
  test("sends with replyableId and optional reason", async () => {
    const h = harness();
    h.messages.value = [
      { id: "msg-1", replyableId: "stanza-by-room", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.moderateMessage("msg-1", "spam");
    const sendModeration = h.xmppClient.value!.sendModeration as unknown as ReturnType<typeof mock>;
    expect(sendModeration).toHaveBeenCalledTimes(1);
    const args = sendModeration.mock.calls[0]!;
    expect(args[2]).toBe("stanza-by-room");
    expect(args[3]).toBe("spam");
  });

  test("refuses without replyableId", async () => {
    const h = harness();
    h.messages.value = [
      { id: "msg-1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    await h.actions.moderateMessage("msg-1");
    expect((h.xmppClient.value!.sendModeration as unknown as ReturnType<typeof mock>).mock.calls.length).toBe(0);
    expect(h.actionError.value).toContain("no room stanza-id");
  });
});

describe("invokeExtensionAction", () => {
  test("forwards launch metadata to the wasm client and returns the result", async () => {
    const invokeExtensionLaunch = mock(async () => ({ ok: true, value: "abc" }));
    const client = makeClient({ invokeExtensionLaunch } as Partial<BrowserXmppClient>);
    const h = harness({ client });

    const result = await h.actions.invokeExtensionAction({
      annotationId: "a1",
      extensionId: "ext",
      label: "Do it",
      launch: { kind: "launch", url: "https://example.com/x" },
    } as any);

    expect(invokeExtensionLaunch).toHaveBeenCalledTimes(1);
    expect(result).toEqual({ ok: true, value: "abc" });
    expect(h.actionError.value).toBe("");
  });

  test("throws and sets actionError when no client is available", async () => {
    const h = harness();
    h.xmppClient.value = null;
    await expect(h.actions.invokeExtensionAction({} as any)).rejects.toThrow("XMPP session is not ready");
    expect(h.actionError.value).toContain("XMPP session is not ready");
  });

  test("throws and sets actionError when launch metadata is missing", async () => {
    const h = harness();
    await expect(h.actions.invokeExtensionAction({ annotationId: "a1", extensionId: "ext", label: "Do it" } as any))
      .rejects.toThrow("missing launch metadata");
    expect(h.actionError.value).toContain("missing launch metadata");
  });
});

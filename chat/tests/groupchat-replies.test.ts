import { describe, test, expect, mock } from "bun:test";
import type { Agent } from "stanza";
import { sendGroupMessage } from "../src/lib/xmpp/messaging";
import { codePointLength } from "../src/lib/text-offsets";

function makeAgent() {
  return {
    sendMessage: mock(() => undefined),
  } as unknown as Agent & {
    sendMessage: ReturnType<typeof mock>;
  };
}

describe("groupchat replies + threads", () => {
  test("attaches reply pointer, fallback range, and thread id", () => {
    const xmpp = makeAgent();

    const messageId = sendGroupMessage(xmpp, "general@muc.waddle.social", "sounds good", {
      replyTo: { id: "msg-1", author: "general@muc.waddle.social/alice", body: "shall we ship?" },
      threadId: "thread-xyz",
    });

    expect(typeof messageId).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;

    const expectedPrefix = "> shall we ship?\n\n";
    expect(call.body).toBe(`${expectedPrefix}sounds good`);
    expect(call.reply).toEqual({ to: "general@muc.waddle.social/alice", id: "msg-1" });
    expect(call.fallbacks).toEqual([
      { for: "urn:xmpp:reply:0", body: { start: 0, end: expectedPrefix.length } },
    ]);
    expect(call.thread).toBe("thread-xyz");
    expect(call.processingHints).toEqual({ store: true });
  });

  test("emits parentThread when supplied alongside threadId", () => {
    const xmpp = makeAgent();

    sendGroupMessage(xmpp, "general@muc.waddle.social", "branching off", {
      threadId: "thread-child",
      parentThreadId: "thread-parent",
    });

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.thread).toBe("thread-child");
    expect(call.parentThread).toBe("thread-parent");
  });

  test("offsets mention references by the reply fallback prefix length", () => {
    const xmpp = makeAgent();

    sendGroupMessage(xmpp, "general@muc.waddle.social", "ping @bob", {
      replyTo: { id: "msg-1", author: "general@muc.waddle.social/alice", body: "hi" },
    });

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    const prefixLength = "> hi\n\n".length;
    expect(call.references).toEqual([
      expect.objectContaining({
        type: "mention",
        uri: "xmpp:bob",
        begin: String(prefixLength + "ping ".length),
        end: String(prefixLength + "ping ".length + "@bob".length),
      }),
    ]);
  });

  test("omits fallback range when the parent body is empty", () => {
    const xmpp = makeAgent();

    sendGroupMessage(xmpp, "general@muc.waddle.social", "standalone reply", {
      replyTo: { id: "msg-1", author: "general@muc.waddle.social/alice" },
    });

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.body).toBe("standalone reply");
    expect(call.reply).toEqual({ to: "general@muc.waddle.social/alice", id: "msg-1" });
    expect(call.fallbacks).toBeUndefined();
  });

  test("omits <thread> when no threadId is supplied (bare reply stays inline)", () => {
    // XEP-0201 threading is opt-in. Plain messages and bare XEP-0461 replies
    // carry no <thread>; only the thread panel composer supplies threadId.
    const xmpp = makeAgent();

    sendGroupMessage(xmpp, "general@muc.waddle.social", "starting a topic");

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.thread).toBeUndefined();
    expect(call.parentThread).toBeUndefined();
  });

  test("omits <thread> on a bare reply (reply and thread are independent)", () => {
    const xmpp = makeAgent();

    sendGroupMessage(xmpp, "general@muc.waddle.social", "inline quote", {
      replyTo: { id: "msg-1", author: "general@muc.waddle.social/alice", body: "hi" },
    });

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.reply).toEqual({ to: "general@muc.waddle.social/alice", id: "msg-1" });
    expect(call.thread).toBeUndefined();
  });

  test("rebases rich markup spans when a reply fallback prefix is prepended", () => {
    const xmpp = makeAgent();
    const body = "bold code link";
    const prefix = "> 👋 hi\n\n";

    sendGroupMessage(xmpp, "general@muc.waddle.social", body, {
      replyTo: { id: "msg-1", author: "general@muc.waddle.social/alice", body: "👋 hi" },
      markup: [
        { type: "span", start: 0, end: codePointLength("bold"), styles: ["strong"] },
        { type: "span", start: codePointLength("bold "), end: codePointLength("bold code"), styles: ["code"] },
      ],
      references: [
        { type: "data", begin: codePointLength("bold code "), end: codePointLength(body), uri: "https://example.com/docs" },
      ],
    });

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    const prefixLength = codePointLength(prefix);
    expect(call.body).toBe(`${prefix}${body}`);
    expect(call.markup).toEqual({
      spans: [
        { type: "span", start: prefixLength, end: prefixLength + codePointLength("bold"), styles: ["strong"] },
        { type: "span", start: prefixLength + codePointLength("bold "), end: prefixLength + codePointLength("bold code"), styles: ["code"] },
      ],
    });
    expect(call.references).toEqual([
      {
        type: "data",
        uri: "https://example.com/docs",
        begin: String(prefixLength + codePointLength("bold code ")),
        end: String(prefixLength + codePointLength(body)),
      },
    ]);
  });
});

import { describe, test, expect, mock } from "bun:test";
import type { Agent } from "stanza";
import { sendDirectMessage } from "../src/lib/xmpp/dm-messaging";

function makeAgent() {
  return {
    sendMessage: mock(() => undefined),
  } as unknown as Agent & {
    sendMessage: ReturnType<typeof mock>;
  };
}

describe("dm replies + threads", () => {
  test("attaches reply pointer, fallback range, and thread id", () => {
    const xmpp = makeAgent();

    const messageId = sendDirectMessage(xmpp, "bob@waddle.social", "sure!", {
      replyTo: { id: "dm-1", author: "bob@waddle.social", body: "want to grab lunch?" },
      threadId: "dm-thread-1",
    });

    expect(typeof messageId).toBe("string");
    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;

    const expectedPrefix = "> want to grab lunch?\n\n";
    expect(call.body).toBe(`${expectedPrefix}sure!`);
    expect(call.reply).toEqual({ to: "bob@waddle.social", id: "dm-1" });
    expect(call.fallbacks).toEqual([
      { for: "urn:xmpp:reply:0", body: { start: 0, end: expectedPrefix.length } },
    ]);
    expect(call.thread).toBe("dm-thread-1");
  });

  test("emits parentThread when supplied alongside threadId", () => {
    const xmpp = makeAgent();

    sendDirectMessage(xmpp, "bob@waddle.social", "branch", {
      threadId: "child",
      parentThreadId: "parent",
    });

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.thread).toBe("child");
    expect(call.parentThread).toBe("parent");
  });

  test("omits fallback range when parent body is unknown", () => {
    const xmpp = makeAgent();

    sendDirectMessage(xmpp, "bob@waddle.social", "replying blind", {
      replyTo: { id: "dm-1", author: "bob@waddle.social" },
    });

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.body).toBe("replying blind");
    expect(call.reply).toEqual({ to: "bob@waddle.social", id: "dm-1" });
    expect(call.fallbacks).toBeUndefined();
  });
});

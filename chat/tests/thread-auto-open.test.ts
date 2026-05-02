import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { findThreadToAutoOpen } from "../src/lib/thread-auto-open";

function message(overrides: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "msg-1",
    author: "alice",
    body: "hello",
    createdAt: "2026-05-01T10:00:00.000Z",
    isSelf: false,
    ...overrides,
  };
}

describe("generic thread auto-open", () => {
  test("opens a thread when a reply to a self-message arrives", () => {
    const root = message({ id: "root-1", isSelf: true });
    const reply = message({ id: "reply-1", threadId: "root-1", isSelf: false });
    const msgs = [root, reply];
    expect(findThreadToAutoOpen(msgs, msgs, new Set())).toBe("root-1");
  });

  test("does not open a thread when the root is not a self-message", () => {
    const root = message({ id: "root-2", isSelf: false });
    const reply = message({ id: "reply-2", threadId: "root-2", isSelf: false });
    const msgs = [root, reply];
    expect(findThreadToAutoOpen(msgs, msgs, new Set())).toBeUndefined();
  });

  test("does not re-open a thread that was already auto-opened", () => {
    const root = message({ id: "root-3", isSelf: true });
    const reply = message({ id: "reply-3", threadId: "root-3", isSelf: false });
    const msgs = [root, reply];
    expect(findThreadToAutoOpen(msgs, msgs, new Set(["root-3"]))).toBeUndefined();
  });

  test("skips root-messages (where id === threadId)", () => {
    const root = message({ id: "root-4", threadId: "root-4", isSelf: true });
    const msgs = [root];
    expect(findThreadToAutoOpen(msgs, msgs, new Set())).toBeUndefined();
  });

  test("skips messages without a threadId", () => {
    const msg = message({ id: "plain-1", isSelf: false });
    const msgs = [msg];
    expect(findThreadToAutoOpen(msgs, msgs, new Set())).toBeUndefined();
  });

  test("returns the first eligible thread when multiple replies arrive", () => {
    const root1 = message({ id: "root-a", isSelf: true });
    const root2 = message({ id: "root-b", isSelf: true });
    const reply1 = message({ id: "reply-a", threadId: "root-a", isSelf: false });
    const reply2 = message({ id: "reply-b", threadId: "root-b", isSelf: false });
    const msgs = [root1, root2, reply1, reply2];
    expect(findThreadToAutoOpen(msgs, msgs, new Set())).toBe("root-a");
  });

  test("does not open a thread when the root message is absent from the loaded timeline", () => {
    const reply = message({ id: "reply-x", threadId: "unknown-root", isSelf: false });
    const msgs = [reply];
    expect(findThreadToAutoOpen(msgs, msgs, new Set())).toBeUndefined();
  });

  test("resolves root via wireId when threadId matches an alternative XMPP id", () => {
    // Simulates the AI-chatbot case: the server sets <thread> to the XMPP
    // wire id, but the frontend's primary id for the same message is the MAM
    // stanza id. The root must still be found and its stable primary id returned.
    const root = message({ id: "mam-stanza-id", isSelf: true, wireIds: ["xmpp-wire-id"] });
    const reply = message({ id: "reply-w", threadId: "xmpp-wire-id", isSelf: false });
    const msgs = [root, reply];
    expect(findThreadToAutoOpen(msgs, msgs, new Set())).toBe("mam-stanza-id");
  });

  test("candidates and allMessages can differ: finds root in allMessages but only triggers on candidates", () => {
    const root = message({ id: "root-z", isSelf: true });
    const historical = message({ id: "hist-z", threadId: "root-z", isSelf: false });
    const liveReply = message({ id: "live-z", threadId: "root-z", isSelf: false });
    const allMessages = [root, historical, liveReply];
    // Only check liveReply as candidate; historical is excluded (simulates initial-load guard)
    expect(findThreadToAutoOpen([liveReply], allMessages, new Set())).toBe("root-z");
    // With historical as the only candidate, same result (thread not yet opened)
    expect(findThreadToAutoOpen([historical], allMessages, new Set())).toBe("root-z");
    // But if already opened, neither triggers
    expect(findThreadToAutoOpen([liveReply], allMessages, new Set(["root-z"]))).toBeUndefined();
  });
});

import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";

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

/**
 * Pure re-implementation of the ContentArea generic thread auto-open logic so
 * the behaviour is independently testable without mounting Vue components.
 */
function findThreadToAutoOpen(
  messages: readonly TimelineMessage[],
  alreadyOpened: ReadonlySet<string>,
): string | undefined {
  for (const msg of messages) {
    if (!msg.threadId || msg.threadId === msg.id) continue;
    if (alreadyOpened.has(msg.threadId)) continue;
    const root = messages.find((m) => m.id === msg.threadId);
    if (!root?.isSelf) continue;
    return msg.threadId;
  }
  return undefined;
}

describe("generic thread auto-open", () => {
  test("opens a thread when a reply to a self-message arrives", () => {
    const root = message({ id: "root-1", isSelf: true });
    const reply = message({ id: "reply-1", threadId: "root-1", isSelf: false });
    expect(findThreadToAutoOpen([root, reply], new Set())).toBe("root-1");
  });

  test("does not open a thread when the root is not a self-message", () => {
    const root = message({ id: "root-2", isSelf: false });
    const reply = message({ id: "reply-2", threadId: "root-2", isSelf: false });
    expect(findThreadToAutoOpen([root, reply], new Set())).toBeUndefined();
  });

  test("does not re-open a thread that was already auto-opened", () => {
    const root = message({ id: "root-3", isSelf: true });
    const reply = message({ id: "reply-3", threadId: "root-3", isSelf: false });
    expect(findThreadToAutoOpen([root, reply], new Set(["root-3"]))).toBeUndefined();
  });

  test("skips root-messages (where id === threadId)", () => {
    const root = message({ id: "root-4", threadId: "root-4", isSelf: true });
    expect(findThreadToAutoOpen([root], new Set())).toBeUndefined();
  });

  test("skips messages without a threadId", () => {
    const msg = message({ id: "plain-1", isSelf: false });
    expect(findThreadToAutoOpen([msg], new Set())).toBeUndefined();
  });

  test("returns the first eligible thread when multiple replies arrive", () => {
    const root1 = message({ id: "root-a", isSelf: true });
    const root2 = message({ id: "root-b", isSelf: true });
    const reply1 = message({ id: "reply-a", threadId: "root-a", isSelf: false });
    const reply2 = message({ id: "reply-b", threadId: "root-b", isSelf: false });
    const result = findThreadToAutoOpen([root1, root2, reply1, reply2], new Set());
    expect(result).toBe("root-a");
  });

  test("does not open a thread when the root message is absent from the loaded timeline", () => {
    const reply = message({ id: "reply-x", threadId: "unknown-root", isSelf: false });
    expect(findThreadToAutoOpen([reply], new Set())).toBeUndefined();
  });
});

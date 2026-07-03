import { describe, expect, test } from "bun:test";
import {
  chronologicalThreadReplies,
  newestThreadMessageId,
  olderRepliesSentinelPosition,
  orderThreadChildren,
  orderThreadMessages,
  threadEdgeMessage,
} from "../src/components/chat/thread-panel-messages";
import type { MessageThreadEntry } from "../src/channels/threads";
import type { TimelineMessage } from "../src/lib/chat-ui";

function message(partial: Partial<TimelineMessage> & { id: string }): TimelineMessage {
  return {
    author: "alice",
    body: "",
    createdAt: "2026-01-01T00:00:00Z",
    isSelf: false,
    ...partial,
  };
}

function entry(partial: Partial<MessageThreadEntry> = {}): MessageThreadEntry {
  const directChildren = partial.directChildren ?? [];
  return {
    threadId: "root",
    root: message({ id: "root", createdAt: "2026-01-01T00:00:00Z" }),
    directChildren,
    allDescendants: directChildren,
    count: directChildren.length,
    lastTs: directChildren.at(-1)?.createdAt ?? "2026-01-01T00:00:00Z",
    ...partial,
  };
}

const replyA = message({ id: "a", threadId: "root", createdAt: "2026-01-01T00:01:00Z" });
const replyB = message({ id: "b", threadId: "root", createdAt: "2026-01-01T00:02:00Z" });

describe("orderThreadChildren", () => {
  test("keeps chronological order in chat mode and reverses in social mode", () => {
    const e = entry({ directChildren: [replyA, replyB] });
    expect(orderThreadChildren(e, "chat").map((m) => m.id)).toEqual(["a", "b"]);
    expect(orderThreadChildren(e, "social").map((m) => m.id)).toEqual(["b", "a"]);
  });

  test("returns an empty list without an entry", () => {
    expect(orderThreadChildren(null, "chat")).toEqual([]);
  });
});

describe("orderThreadMessages", () => {
  test("places the root at the oldest end for each mode", () => {
    const e = entry({ directChildren: [replyA, replyB] });
    const chatChildren = orderThreadChildren(e, "chat");
    expect(orderThreadMessages(e, chatChildren, "chat").map((m) => m.id)).toEqual([
      "root",
      "a",
      "b",
    ]);
    const socialChildren = orderThreadChildren(e, "social");
    expect(orderThreadMessages(e, socialChildren, "social").map((m) => m.id)).toEqual([
      "b",
      "a",
      "root",
    ]);
  });

  test("returns a copy of the children when the root is missing", () => {
    const e = entry({ root: undefined, directChildren: [replyA] });
    const children = orderThreadChildren(e, "chat");
    const rendered = orderThreadMessages(e, children, "chat");
    expect(rendered.map((m) => m.id)).toEqual(["a"]);
    expect(rendered).not.toBe(children);
  });
});

describe("chronologicalThreadReplies", () => {
  test("keeps only replies belonging to the active thread", () => {
    const nested = message({ id: "nested", threadId: "child", createdAt: "2026-01-01T00:03:00Z" });
    const e = entry({ directChildren: [replyA, nested, replyB] });
    expect(chronologicalThreadReplies(e, "root").map((m) => m.id)).toEqual(["a", "b"]);
  });

  test("returns empty without an entry or thread id", () => {
    expect(chronologicalThreadReplies(null, "root")).toEqual([]);
    expect(chronologicalThreadReplies(entry(), null)).toEqual([]);
  });
});

describe("newestThreadMessageId", () => {
  test("prefers the newest reply, falls back to the root, then null", () => {
    expect(newestThreadMessageId(entry({ directChildren: [replyA, replyB] }))).toBe("b");
    expect(newestThreadMessageId(entry())).toBe("root");
    expect(newestThreadMessageId(entry({ root: undefined }))).toBeNull();
    expect(newestThreadMessageId(null)).toBeNull();
  });
});

describe("olderRepliesSentinelPosition", () => {
  test("chat mode paginates at the start", () => {
    expect(olderRepliesSentinelPosition("chat", true)).toBe("start");
    expect(olderRepliesSentinelPosition("chat", false)).toBe("start");
  });

  test("social mode sits inboard of a trailing root", () => {
    expect(olderRepliesSentinelPosition("social", true)).toBe("before-end");
    expect(olderRepliesSentinelPosition("social", false)).toBe("end");
  });
});

describe("threadEdgeMessage", () => {
  test("top-pinned edge is the newest rendered message", () => {
    const e = entry({ directChildren: [replyA, replyB] });
    const children = orderThreadChildren(e, "social");
    const rendered = orderThreadMessages(e, children, "social");
    expect(threadEdgeMessage(children, rendered, e.root ?? null, "social")?.id).toBe("b");
  });

  test("bottom-pinned edge is the last rendered message", () => {
    const e = entry({ directChildren: [replyA, replyB] });
    const children = orderThreadChildren(e, "chat");
    const rendered = orderThreadMessages(e, children, "chat");
    expect(threadEdgeMessage(children, rendered, e.root ?? null, "chat")?.id).toBe("b");
  });

  test("falls back to the root when no replies are rendered", () => {
    const e = entry();
    const children = orderThreadChildren(e, "social");
    const rendered = orderThreadMessages(e, children, "social");
    expect(threadEdgeMessage(children, rendered, e.root ?? null, "social")?.id).toBe("root");
    expect(threadEdgeMessage([], [], null, "chat")).toBeNull();
  });
});

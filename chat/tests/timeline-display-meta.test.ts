import { describe, expect, test } from "bun:test";
import {
  buildMessageDisplayMeta,
  threadChipLastReplyAt,
  threadChipParticipants,
} from "../src/components/chat/timeline-display-meta";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { MessageThreadEntry, MessageThreadIndex } from "../src/channels/threads";

function message(partial: Partial<TimelineMessage> & { id: string; author: string; createdAt: string }): TimelineMessage {
  return {
    body: "",
    isSelf: false,
    createdAtSource: "archive",
    ...partial,
  } as TimelineMessage;
}

describe("buildMessageDisplayMeta", () => {
  test("groups same-author bursts inside the five-minute window", () => {
    const meta = buildMessageDisplayMeta([
      message({ id: "a1", author: "alice", createdAt: "2026-07-01T10:00:00Z" }),
      message({ id: "a2", author: "alice", createdAt: "2026-07-01T10:03:00Z" }),
      message({ id: "a3", author: "alice", createdAt: "2026-07-01T10:09:00Z" }),
      message({ id: "b1", author: "bob", createdAt: "2026-07-01T10:10:00Z" }),
      message({ id: "a4", author: "alice", createdAt: "2026-07-01T10:11:00Z" }),
    ]);
    expect(meta.grouped.has("a2")).toBe(true);
    // > 5 min gap breaks the burst.
    expect(meta.grouped.has("a3")).toBe(false);
    // Author change breaks the burst even when close in time.
    expect(meta.grouped.has("b1")).toBe(false);
    expect(meta.grouped.has("a4")).toBe(false);
  });

  test("marks day dividers on day boundaries and never groups across them", () => {
    const meta = buildMessageDisplayMeta([
      message({ id: "d1", author: "alice", createdAt: "2026-07-01T23:59:00" }),
      message({ id: "d2", author: "alice", createdAt: "2026-07-02T00:01:00" }),
    ]);
    expect(meta.dayDivider.has("d2")).toBe(true);
    expect(meta.grouped.has("d2")).toBe(false);
  });
});

function threadIndexWith(children: TimelineMessage[]): MessageThreadIndex {
  const entry: MessageThreadEntry = {
    threadId: "root",
    root: message({ id: "root", author: "alice", createdAt: "2026-07-01T09:00:00Z" }),
    directChildren: children,
    allDescendants: children,
    count: children.length,
    lastTs: children.at(-1)?.createdAt ?? "",
  };
  return new Map([["root", entry]]);
}

describe("threadChipParticipants", () => {
  test("walks newest-first, dedups authors, and caps at five", () => {
    const children = ["u1", "u2", "u3", "u2", "u4", "u5", "u6", "u7"].map((author, i) =>
      message({ id: `c${i}`, author, createdAt: `2026-07-01T10:0${i}:00Z` }),
    );
    const participants = threadChipParticipants(
      threadIndexWith(children),
      "root",
      { u7: "https://cdn/u7.png" },
      { u7: "online" },
    );
    expect(participants.map((p) => p.nick)).toEqual(["u7", "u6", "u5", "u4", "u2"]);
    expect(participants[0]).toEqual({ nick: "u7", avatarUrl: "https://cdn/u7.png", presence: "online" });
    expect(participants[1]?.presence).toBe("offline");
  });

  test("returns empty for unknown threads", () => {
    expect(threadChipParticipants(new Map(), "missing", {}, {})).toEqual([]);
  });
});

describe("threadChipLastReplyAt", () => {
  test("returns the newest direct child's timestamp", () => {
    const children = [
      message({ id: "c0", author: "u1", createdAt: "2026-07-01T10:00:00Z" }),
      message({ id: "c1", author: "u2", createdAt: "2026-07-01T10:05:00Z" }),
    ];
    expect(threadChipLastReplyAt(threadIndexWith(children), "root")).toBe("2026-07-01T10:05:00Z");
    expect(threadChipLastReplyAt(new Map(), "root")).toBeUndefined();
  });
});

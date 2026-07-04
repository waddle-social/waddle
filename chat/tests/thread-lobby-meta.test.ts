import { describe, expect, test } from "bun:test";
import {
  formatThreadLastActivity,
  overflowThreadParticipantCount,
  threadBreadcrumbLabels,
  threadLastActivityFor,
  threadParticipantsFor,
  threadPreviewFor,
  visibleThreadParticipants,
} from "../src/components/chat/thread-lobby-meta";
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
    root: message({ id: "root", author: "root-author", body: "root body" }),
    directChildren,
    allDescendants: directChildren,
    count: directChildren.length,
    lastTs: "2026-01-01T00:00:00Z",
    ...partial,
  };
}

function isoAgo(ms: number): string {
  return new Date(Date.now() - ms).toISOString();
}

describe("threadBreadcrumbLabels", () => {
  test("uses the trimmed root body capped at 40 chars, falling back to the id prefix", () => {
    const longBody = "x".repeat(60);
    const entries = new Map<string, MessageThreadEntry>([
      ["thread-with-body", entry({ root: message({ id: "r1", body: `  ${longBody}  ` }) })],
      ["thread-empty-body", entry({ root: message({ id: "r2", body: "   " }) })],
    ]);
    const labels = threadBreadcrumbLabels(
      ["thread-with-body", "thread-empty-body", "unresolved-thread-id"],
      (id) => entries.get(id),
    );
    expect(labels).toEqual(["x".repeat(40), "thread-e", "unresolv"]);
  });
});

describe("threadPreviewFor", () => {
  test("returns the trimmed body when short enough", () => {
    expect(threadPreviewFor(entry({ root: message({ id: "r", body: "  hello  " }) }))).toBe("hello");
  });

  test("truncates long bodies at 95 chars with an ellipsis", () => {
    const body = "a".repeat(200);
    const preview = threadPreviewFor(entry({ root: message({ id: "r", body }) }));
    expect(preview).toBe(`${"a".repeat(95)}…`);
  });

  test("is empty without an entry, root, or body", () => {
    expect(threadPreviewFor(null)).toBe("");
    expect(threadPreviewFor(entry({ root: undefined }))).toBe("");
    expect(threadPreviewFor(entry({ root: message({ id: "r", body: "" }) }))).toBe("");
  });
});

describe("threadParticipantsFor", () => {
  const avatars = { alice: "alice.png", bob: null } as Record<string, string | null>;
  const presence = { alice: "online" } as Record<string, "online" | "away" | "dnd" | "offline">;

  test("orders unique reply authors newest-first and appends the root author", () => {
    const e = entry({
      root: message({ id: "root", author: "root-author" }),
      directChildren: [
        message({ id: "1", author: "alice" }),
        message({ id: "2", author: "bob" }),
        message({ id: "3", author: "alice" }),
      ],
    });
    const participants = threadParticipantsFor(e, avatars, presence);
    expect(participants.map((p) => p.nick)).toEqual(["alice", "bob", "root-author"]);
    expect(participants[0]).toEqual({ nick: "alice", avatarUrl: "alice.png", presence: "online" });
    expect(participants[1]).toEqual({ nick: "bob", avatarUrl: null, presence: "offline" });
  });

  test("does not duplicate a root author who also replied", () => {
    const e = entry({
      root: message({ id: "root", author: "alice" }),
      directChildren: [message({ id: "1", author: "alice" })],
    });
    expect(threadParticipantsFor(e, avatars, presence).map((p) => p.nick)).toEqual(["alice"]);
  });

  test("is empty without an entry", () => {
    expect(threadParticipantsFor(null, avatars, presence)).toEqual([]);
  });
});

describe("visibleThreadParticipants / overflowThreadParticipantCount", () => {
  test("caps the avatar stack at four and counts the rest", () => {
    const participants = ["a", "b", "c", "d", "e", "f"].map((nick) => ({
      nick,
      presence: "offline" as const,
    }));
    expect(visibleThreadParticipants(participants).map((p) => p.nick)).toEqual(["a", "b", "c", "d"]);
    expect(overflowThreadParticipantCount(participants)).toBe(2);
    expect(overflowThreadParticipantCount(participants.slice(0, 3))).toBe(0);
  });
});

describe("threadLastActivityFor", () => {
  test("prefers the newest reply, falls back to the root", () => {
    const e = entry({
      root: message({ id: "root", createdAt: "2026-01-01T00:00:00Z" }),
      directChildren: [message({ id: "1", createdAt: "2026-01-02T00:00:00Z" })],
    });
    expect(threadLastActivityFor(e)).toBe("2026-01-02T00:00:00Z");
    expect(threadLastActivityFor(entry())).toBe("2026-01-01T00:00:00Z");
    expect(threadLastActivityFor(null)).toBeNull();
  });
});

describe("formatThreadLastActivity", () => {
  test("is empty for null, invalid, or future timestamps", () => {
    expect(formatThreadLastActivity(null)).toBe("");
    expect(formatThreadLastActivity("not-a-date")).toBe("");
    expect(formatThreadLastActivity(isoAgo(-60_000))).toBe("");
  });

  test("buckets relative times", () => {
    expect(formatThreadLastActivity(isoAgo(10_000))).toBe("just now");
    expect(formatThreadLastActivity(isoAgo(90_000))).toBe("1 min ago");
    expect(formatThreadLastActivity(isoAgo(5 * 60_000))).toBe("5 min ago");
    expect(formatThreadLastActivity(isoAgo(90 * 60_000))).toBe("1 hour ago");
    expect(formatThreadLastActivity(isoAgo(5 * 3_600_000))).toBe("5 hours ago");
    expect(formatThreadLastActivity(isoAgo(30 * 3_600_000))).toBe("yesterday");
    expect(formatThreadLastActivity(isoAgo(10 * 86_400_000))).toBe("10 days ago");
  });
});

import { describe, expect, test } from "bun:test";
import {
  moveReactionSelection,
  preserveReactionSelection,
  QUICK_REACTION_EMOJIS,
  quickReactionForKey,
  reactionModeMessages,
  selectInitialReactionMessage,
  type ReactionModeMessage,
} from "../src/lib/reaction-mode";

const messages: ReactionModeMessage[] = [
  { id: "oldest", createdAt: "2026-04-29T10:00:00Z" },
  { id: "thread-root", createdAt: "2026-04-29T10:00:30Z", threadId: "thread-root" },
  { id: "thread-child", createdAt: "2026-04-29T10:01:00Z", threadId: "oldest" },
  { id: "retracted", createdAt: "2026-04-29T10:02:00Z", isRetracted: true },
  { id: "queued", createdAt: "2026-04-29T10:02:30Z", deliveryStatus: "queued" },
  { id: "newest-feed", createdAt: "2026-04-29T10:03:00Z" },
];

describe("reaction mode helpers", () => {
  test("exports the quick reaction emoji palette", () => {
    expect(QUICK_REACTION_EMOJIS).toEqual(["👍", "❤️", "😂", "🎉", "👀"]);
  });

  test("selects the most recent eligible feed message initially", () => {
    expect(selectInitialReactionMessage(messages, "feed")).toBe("newest-feed");
  });

  test("filters main feed eligibility separately from thread eligibility", () => {
    expect(reactionModeMessages(messages, "feed").map((message) => message.id)).toEqual([
      "oldest",
      "thread-root",
      "newest-feed",
    ]);

    expect(reactionModeMessages(messages, "thread").map((message) => message.id)).toEqual([
      "oldest",
      "thread-root",
      "thread-child",
      "newest-feed",
    ]);
  });

  test("excludes local-only undelivered messages from reaction mode", () => {
    expect(
      reactionModeMessages([
        { id: "queued", createdAt: "2026-04-29T10:00:00Z", deliveryStatus: "queued" },
        { id: "sending", createdAt: "2026-04-29T10:01:00Z", deliveryStatus: "sending" },
        { id: "failed", createdAt: "2026-04-29T10:02:00Z", deliveryStatus: "failed" },
        { id: "delivered", createdAt: "2026-04-29T10:03:00Z", deliveryStatus: "delivered" },
      ], "feed").map((message) => message.id),
    ).toEqual(["delivered"]);
  });

  test("excludes messages without a conformant reaction target signal", () => {
    expect(
      reactionModeMessages([
        { id: "no-target", createdAt: "2026-04-29T10:00:00Z", canReact: false },
        { id: "target", createdAt: "2026-04-29T10:01:00Z", canReact: true },
      ], "feed").map((message) => message.id),
    ).toEqual(["target"]);
  });

  test("moves selection with arrows over the rendered order", () => {
    const renderedOrder: ReactionModeMessage[] = [
      { id: "first-rendered", createdAt: "2026-04-29T10:03:00Z" },
      { id: "thread-child", createdAt: "2026-04-29T10:04:00Z", threadId: "first-rendered" },
      { id: "second-rendered", createdAt: "2026-04-29T10:01:00Z" },
      { id: "retracted", createdAt: "2026-04-29T10:05:00Z", isRetracted: true },
      { id: "third-rendered", createdAt: "2026-04-29T10:02:00Z" },
    ];

    expect(moveReactionSelection("second-rendered", renderedOrder, "feed", "previous")).toBe("first-rendered");
    expect(moveReactionSelection("second-rendered", renderedOrder, "feed", "next")).toBe("third-rendered");
    expect(moveReactionSelection("first-rendered", renderedOrder, "feed", "previous")).toBe("first-rendered");
    expect(moveReactionSelection("third-rendered", renderedOrder, "feed", "next")).toBe("third-rendered");
    expect(moveReactionSelection("first-rendered", renderedOrder, "thread", "next")).toBe("thread-child");
  });

  test("preserves a still-eligible selection and falls back to the initial selection", () => {
    expect(preserveReactionSelection("oldest", messages, "feed")).toBe("oldest");
    expect(preserveReactionSelection("thread-child", messages, "feed")).toBe("newest-feed");
    expect(preserveReactionSelection(null, messages, "feed")).toBe("newest-feed");
  });

  test("maps number keys 1-5 to quick reactions", () => {
    expect(["1", "2", "3", "4", "5"].map((key) => quickReactionForKey(key))).toEqual([
      "👍",
      "❤️",
      "😂",
      "🎉",
      "👀",
    ]);
    expect(quickReactionForKey("0")).toBeNull();
    expect(quickReactionForKey("6")).toBeNull();
  });

});

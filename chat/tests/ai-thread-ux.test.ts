import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";
import {
  aiComposerCommandResults,
  filterV1ExtensionPaletteCommands,
  isAiThreadPromptBody,
  nextAiThreadRootToOpen,
  withAiAssistantMentionCandidate,
} from "../src/lib/ai-thread-ux";

function message(overrides: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "msg-1",
    author: "alice",
    body: "hello",
    createdAt: "2026-05-01T10:00:00.000Z",
    isSelf: true,
    ...overrides,
  };
}

describe("AI thread UX", () => {
  test("detects /ai prompts and @waddle mentions", () => {
    expect(isAiThreadPromptBody("/ai summarize this")).toBe(true);
    expect(isAiThreadPromptBody("  /ai")).toBe(true);
    expect(isAiThreadPromptBody("can @waddle help?")).toBe(true);
    expect(isAiThreadPromptBody("plain message")).toBe(false);
    expect(isAiThreadPromptBody("prefix /ai in the middle")).toBe(false);
  });

  test("opens the new self-authored main-feed root for a pending AI prompt", () => {
    const seen = new Set(["old-ai"]);
    const result = nextAiThreadRootToOpen(
      [
        message({ id: "old-ai", body: "/ai old", deliveryStatus: "delivered" }),
        message({ id: "new-ai", body: "/ai summarize this", deliveryStatus: "delivered" }),
      ],
      ["/ai summarize this"],
      seen,
    );

    expect(result).toEqual({ messageId: "new-ai", promptIndex: 0 });
  });

  test("does not open threads for replies, thread children, other users, or already-seen messages", () => {
    const pending = ["@waddle summarize"];
    const seen = new Set(["seen-root"]);

    expect(nextAiThreadRootToOpen([
      message({ id: "seen-root", body: "@waddle summarize" }),
      message({ id: "reply", body: "@waddle summarize", deliveryStatus: "delivered", replyTo: { id: "root" } }),
      message({ id: "child", body: "@waddle summarize", deliveryStatus: "delivered", threadId: "root" }),
      message({ id: "other-user", body: "@waddle summarize", deliveryStatus: "delivered", isSelf: false }),
    ], pending, seen)).toBeUndefined();
  });

  test("waits for the canonical delivered self-echo before opening", () => {
    const pending = ["/ai summarize this"];

    expect(nextAiThreadRootToOpen([
      message({ id: "client-id", body: "/ai summarize this", deliveryStatus: "sending" }),
    ], pending, new Set())).toBeUndefined();

    expect(nextAiThreadRootToOpen([
      message({ id: "canonical-room-id", body: "/ai summarize this", deliveryStatus: "delivered" }),
    ], pending, new Set(["client-id"]))).toEqual({ messageId: "canonical-room-id", promptIndex: 0 });
  });

  test("hides the AI chatbot command from v1 extension palette discovery", () => {
    expect(filterV1ExtensionPaletteCommands([
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:poll", name: "Poll" },
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:1:ai-chatbot", name: "Ask AI Chatbot" },
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:notes", name: "Notes" },
    ])).toEqual([
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:poll", name: "Poll" },
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:notes", name: "Notes" },
    ]);
  });

  test("offers the AI slash command for composer autocomplete", () => {
    expect(aiComposerCommandResults("", true).map((command) => command.command)).toEqual(["/ai"]);
    expect(aiComposerCommandResults("a", true).map((command) => command.command)).toEqual(["/ai"]);
    expect(aiComposerCommandResults("poll", true)).toEqual([]);
    expect(aiComposerCommandResults("", false)).toEqual([]);
  });

  test("adds the Waddle assistant to mention autocomplete once", () => {
    expect(withAiAssistantMentionCandidate([], true)).toEqual([
      {
        username: "waddle",
        jid: null,
        avatar_url: null,
        kind: "member",
      },
    ]);
    expect(withAiAssistantMentionCandidate([], false)).toEqual([]);

    expect(withAiAssistantMentionCandidate([
      {
        username: "Waddle",
        jid: "waddle@example.com",
        avatar_url: null,
        kind: "member",
      },
    ], true)).toHaveLength(1);
  });
});

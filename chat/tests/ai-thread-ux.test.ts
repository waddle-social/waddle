import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";
import {
  aiComposerCommandResults,
  filterV1ExtensionPaletteCommands,
  findBotReplyThreadToOpen,
  withAiAssistantMentionCandidate,
} from "../src/lib/ai-thread-ux";

function message(overrides: Partial<TimelineMessage> & { id: string }): TimelineMessage {
  return {
    author: "alice",
    body: "hello",
    createdAt: "2026-05-01T10:00:00.000Z",
    isSelf: true,
    ...overrides,
  };
}

describe("AI thread UX", () => {
  describe("findBotReplyThreadToOpen", () => {
    test("opens thread when a live bot reply arrives for a self-authored root", () => {
      const seen = new Set(["prompt-1"]);
      const result = findBotReplyThreadToOpen(
        [
          message({ id: "prompt-1", isSelf: true }),
          message({ id: "bot-reply-1", isSelf: false, threadId: "prompt-1" }),
        ],
        seen,
      );
      expect(result).toBe("prompt-1");
    });

    test("ignores messages already in seenMessageIds", () => {
      const seen = new Set(["prompt-1", "bot-reply-1"]);
      expect(findBotReplyThreadToOpen(
        [
          message({ id: "prompt-1", isSelf: true }),
          message({ id: "bot-reply-1", isSelf: false, threadId: "prompt-1" }),
        ],
        seen,
      )).toBeUndefined();
    });

    test("ignores self-authored replies (only opens for others)", () => {
      const seen = new Set<string>();
      expect(findBotReplyThreadToOpen(
        [
          message({ id: "root", isSelf: true }),
          message({ id: "self-reply", isSelf: true, threadId: "root" }),
        ],
        seen,
      )).toBeUndefined();
    });

    test("ignores thread roots (id === threadId)", () => {
      const seen = new Set<string>();
      expect(findBotReplyThreadToOpen(
        [
          message({ id: "root", isSelf: false, threadId: "root" }),
        ],
        seen,
      )).toBeUndefined();
    });

    test("requires the thread root to be self-authored and a feed message", () => {
      const seen = new Set<string>();
      // Root is from another user — don't open.
      expect(findBotReplyThreadToOpen(
        [
          message({ id: "other-root", isSelf: false }),
          message({ id: "bot-reply", isSelf: false, threadId: "other-root" }),
        ],
        seen,
      )).toBeUndefined();
    });

    test("ignores replies when thread root is not in the message list", () => {
      const seen = new Set<string>();
      expect(findBotReplyThreadToOpen(
        [
          message({ id: "bot-reply", isSelf: false, threadId: "missing-root" }),
        ],
        seen,
      )).toBeUndefined();
    });
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

import { describe, expect, test } from "bun:test";
import { insertLiveMessage } from "../src/lib/messaging/timeline-insert";
import type { TimelineMessage } from "../src/lib/chat-ui";

function row(overrides: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "m1",
    author: "sam",
    authorJid: "room@muc.example.com/sam",
    authorOccupantJid: "room@muc.example.com/sam",
    body: "hi",
    createdAt: "2026-09-27T10:00:00Z",
    createdAtSource: "fallback",
    isSelf: false,
    ...overrides,
  };
}

describe("live merge keeps the first avatar attribution", () => {
  test("a redelivered copy stamped with a later nick holder does not overwrite the original stamp", () => {
    const first = insertLiveMessage([row({ authorAvatarJid: "alice@example.com" })], row({ authorAvatarJid: "bob@example.com" }), new Set());
    expect(first.messages).toHaveLength(1);
    expect(first.messages[0]?.authorAvatarJid).toBe("alice@example.com");
  });

  test("an unstamped row adopts the stamp of a redelivered copy", () => {
    const merged = insertLiveMessage([row({})], row({ authorAvatarJid: "alice@example.com" }), new Set());
    expect(merged.messages[0]?.authorAvatarJid).toBe("alice@example.com");
  });
});

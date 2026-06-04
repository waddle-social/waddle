import { describe, expect, test } from "bun:test";
import {
  displayedStateCanAdvance,
  firstUnseenIdAfterDisplayedState,
  firstUnseenIdFromUnreadCount,
} from "../src/lib/displayed-state";
import type { TimelineMessage } from "../src/lib/chat-ui";

function message(id: string, stanzaId?: string, stanzaIdBy?: string, isSelf = false): TimelineMessage {
  return {
    id,
    ...(stanzaId ? { stanzaId } : {}),
    ...(stanzaIdBy ? { stanzaIdBy } : {}),
    author: "bob",
    body: "",
    createdAt: "2024-01-01T00:00:00Z",
    createdAtSource: "archive",
    isSelf,
  } as TimelineMessage;
}

describe("displayed state divider helpers", () => {
  test("advances first unseen only when displayed state reaches unread messages", () => {
    const timeline = [
      message("m1", "s1", "example.com"),
      message("m2", "s2", "example.com"),
      message("m3", "s3", "example.com"),
    ];
    const firstUnread = firstUnseenIdFromUnreadCount(timeline, 2);

    expect(firstUnread).toBe("m2");
    expect(firstUnseenIdAfterDisplayedState(timeline, firstUnread, { stanzaId: "s1", stanzaIdBy: "example.com" })).toBe("m2");
    expect(firstUnseenIdAfterDisplayedState(timeline, firstUnread, { stanzaId: "s2", stanzaIdBy: "example.com" })).toBe("m3");
    expect(firstUnseenIdAfterDisplayedState(timeline, firstUnread, { stanzaId: "s3", stanzaIdBy: "example.com" })).toBeNull();
    expect(firstUnseenIdAfterDisplayedState(timeline, firstUnread, { stanzaId: "missing", stanzaIdBy: "example.com" })).toBe("m2");
  });

  test("resolves XEP-0490 displayed state by stanza-id and by JID", () => {
    const timeline = [
      message("sender-id-1", "server-stanza-1", "example.com"),
      message("sender-id-2", "server-stanza-2", "example.com"),
    ];
    const firstUnread = firstUnseenIdFromUnreadCount(timeline, 2);

    expect(firstUnseenIdAfterDisplayedState(
      timeline,
      firstUnread,
      { stanzaId: "server-stanza-1", stanzaIdBy: "example.com" },
    )).toBe("sender-id-2");
    expect(firstUnseenIdAfterDisplayedState(
      timeline,
      firstUnread,
      { stanzaId: "server-stanza-1", stanzaIdBy: "other.example" },
    )).toBe("sender-id-1");
  });

  test("unread count ignores self-authored tail messages", () => {
    const timeline = [
      message("remote-1", "s1", "example.com"),
      message("self-1", "self-s1", "example.com", true),
      message("remote-2", "s2", "example.com"),
    ];

    expect(firstUnseenIdFromUnreadCount(timeline, 1)).toBe("remote-2");
    expect(firstUnseenIdFromUnreadCount(timeline, 2)).toBe("remote-1");
  });

  test("displayed state only advances forward on a resolvable timeline", () => {
    const timeline = [
      message("m1", "s1", "example.com"),
      message("m2", "s2", "example.com"),
    ];

    expect(displayedStateCanAdvance(
      timeline,
      { stanzaId: "s2", stanzaIdBy: "example.com" },
      { stanzaId: "s1", stanzaIdBy: "example.com" },
    )).toBe(false);
    expect(displayedStateCanAdvance(
      timeline,
      { stanzaId: "s1", stanzaIdBy: "example.com" },
      { stanzaId: "s2", stanzaIdBy: "example.com" },
    )).toBe(true);
    expect(displayedStateCanAdvance(
      timeline,
      null,
      { stanzaId: "missing", stanzaIdBy: "example.com" },
    )).toBe(false);
  });
});

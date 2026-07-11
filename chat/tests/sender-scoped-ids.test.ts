import { describe, expect, test } from "bun:test";

import type { TimelineMessage } from "@/lib/chat-ui";
import { SenderScopedIdIndex } from "@/lib/messaging/sender-scoped-ids";

function canonicalRoomMessage(index: number): TimelineMessage {
  const stanzaId = `room-stanza-${index}`;
  return {
    id: stanzaId,
    wireIds: ["reused-client-id"],
    stanzaId,
    stanzaIdBy: "room@muc.example.com",
    author: "alice",
    authorJid: "alice@example.com/phone",
    authorOccupantJid: "room@muc.example.com/alice",
    authorRealJid: "alice@example.com/phone",
    body: `message ${index}`,
    isSelf: false,
    createdAt: "2026-05-14T10:36:55Z",
    createdAtSource: "archive",
  };
}

describe("SenderScopedIdIndex", () => {
  test("preserves fail-closed multiplicity for a repeated object reference", () => {
    const message = canonicalRoomMessage(0);
    const index = new SenderScopedIdIndex([message, message]);

    expect(index.find(message)).toBeUndefined();
  });

  test("bounds resolution work when one sender reuses an alias across canonical messages", () => {
    const messageCount = 2_000;
    const index = new SenderScopedIdIndex();

    for (let candidate = 0; candidate < messageCount; candidate += 1) {
      const message = canonicalRoomMessage(candidate);
      expect(index.find(message)).toBeUndefined();
      index.add(message);
    }

    expect(index.resolutionProbeCount).toBeLessThanOrEqual(messageCount * 12);
  });

  test("prunes canonical partitions after repeated replacement", () => {
    const index = new SenderScopedIdIndex();
    let current = canonicalRoomMessage(0);
    index.add(current);

    for (let candidate = 1; candidate <= 2_000; candidate += 1) {
      const replacement = canonicalRoomMessage(candidate);
      index.replace(current, replacement);
      current = replacement;
    }

    expect(index.retainedCanonicalPartitionCount).toBe(3);
  });
});

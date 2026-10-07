import { describe, expect, test } from "bun:test";

import type { TimelineMessage } from "@/lib/chat-ui";
import {
  findSenderScopedIdTarget,
  SenderScopedIdIndex,
} from "@/lib/messaging/sender-scoped-ids";

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
  test("normalizes bare sender JIDs identically in the scan and index", () => {
    const existing: TimelineMessage = {
      ...canonicalRoomMessage(0),
      author: "Alice",
      authorOccupantJid: undefined,
      authorRealJid: undefined,
      authorJid: " Alice@Example.COM/phone ",
    };
    const incoming: TimelineMessage = {
      ...existing,
      author: "alice",
      authorJid: "alice@example.com/laptop",
    };

    expect(findSenderScopedIdTarget([existing], incoming)).toBe(existing);
    expect(new SenderScopedIdIndex([existing]).find(incoming)).toBe(existing);
  });

  test("fails closed when only a display nick identifies the sender", () => {
    const existing: TimelineMessage = {
      ...canonicalRoomMessage(0),
      authorOccupantJid: undefined,
      authorRealJid: undefined,
      authorJid: undefined,
    };
    const incoming: TimelineMessage = { ...existing };

    expect(findSenderScopedIdTarget([existing], incoming)).toBeUndefined();
    expect(new SenderScopedIdIndex([existing]).find(incoming)).toBeUndefined();
  });


  test("room sender IDs require real identity and survive nick changes", () => {
    const known = { ...canonicalRoomMessage(0), stanzaId: undefined, stanzaIdBy: undefined };
    const unknown = { ...known, authorRealJid: undefined };
    const scenarios: [TimelineMessage, TimelineMessage, boolean][] = [
      [unknown, { ...unknown }, false],
      [unknown, known, false],
      [known, unknown, false],
      [known, { ...known, authorRealJid: "mallory@example.com" }, false],
      [known, { ...known, authorOccupantJid: "room@muc.example.com/new-nick", authorRealJid: "ALICE@EXAMPLE.COM/laptop" }, true],
      [unknown, { ...unknown, authorAvatarJid: "alice@example.com" }, false],
      [known, { ...known, authorOccupantJid: "elsewhere@muc.example.com/alice" }, false],
    ];
    for (const [existing, incoming, matches] of scenarios) {
      expect(findSenderScopedIdTarget([existing], incoming)).toBe(matches ? existing : undefined);
      expect(new SenderScopedIdIndex([existing]).find(incoming)).toBe(matches ? existing : undefined);
    }
  });

  test("room canonical identity resolves before sender metadata or sender aliases", () => {
    const existing = canonicalRoomMessage(0);
    const incoming = {
      ...existing,
      id: "different-client-id",
      wireIds: ["unrelated-alias"],
      authorOccupantJid: "room@muc.example.com/new-nick",
      authorRealJid: "different@example.com",
    };
    expect(findSenderScopedIdTarget([existing], incoming)).toBe(existing);
    expect(new SenderScopedIdIndex([existing]).find(incoming)).toBe(existing);
  });

  test("different room canonical tuples block a shared sender alias", () => {
    const existing = canonicalRoomMessage(0);
    const incoming = { ...canonicalRoomMessage(1), id: existing.id };
    expect(findSenderScopedIdTarget([existing], incoming)).toBeUndefined();
    expect(new SenderScopedIdIndex([existing]).find(incoming)).toBeUndefined();
  });


  test("foreign stanza-id authority cannot authenticate an unknown room sender", () => {
    const existing = { ...canonicalRoomMessage(0), authorRealJid: undefined, stanzaIdBy: "foreign.example.com" };
    const incoming = { ...existing };
    expect(findSenderScopedIdTarget([existing], incoming)).toBeUndefined();
    expect(new SenderScopedIdIndex([existing]).find(incoming)).toBeUndefined();
  });

  test("canonical tuple ambiguity fails closed even when a sender alias is unique", () => {
    const first = canonicalRoomMessage(0);
    const second = { ...first, id: "other-client-id", authorRealJid: "bob@example.com" };
    expect(findSenderScopedIdTarget([first, second], first)).toBeUndefined();
    expect(new SenderScopedIdIndex([first, second]).find(first)).toBeUndefined();
  });

  test("sender IDs cannot collide with room-assigned or archive identities", () => {
    const canonical = { ...canonicalRoomMessage(0), originId: "first-origin", correctionTargetId: "first-origin" };
    const incoming = { ...canonical, stanzaId: undefined, stanzaIdBy: undefined, wireIds: [], originId: canonical.id, correctionTargetId: canonical.id };
    expect(findSenderScopedIdTarget([canonical], incoming)).toBeUndefined();
    expect(new SenderScopedIdIndex([canonical]).find(incoming)).toBeUndefined();
  });

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

    expect(index.retainedCanonicalPartitionCount).toBe(2);
  });
});

import { describe, expect, test } from "bun:test";
import {
  compareTimelineMessages,
  compareTimelineTimestamps,
} from "../src/lib/timeline-timestamps";

type SortableMessage = {
  createdAt?: string;
  stanzaId?: string;
  id?: string;
  deliveryStatus?: "queued" | "sending" | "delivered" | "failed";
};

function shuffle<T>(input: T[], seed: number): T[] {
  // Xorshift32 — deterministic, no Math.random in tests.
  let state = seed || 1;
  const next = () => {
    state ^= state << 13; state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5; state >>>= 0;
    return state >>> 0;
  };
  const out = [...input];
  for (let i = out.length - 1; i > 0; i--) {
    const j = next() % (i + 1);
    [out[i], out[j]] = [out[j]!, out[i]!];
  }
  return out;
}

describe("compareTimelineTimestamps", () => {
  test("orders ISO timestamps chronologically", () => {
    expect(compareTimelineTimestamps("2026-05-20T10:00:00.000Z", "2026-05-20T10:00:00.001Z")).toBe(-1);
    expect(compareTimelineTimestamps("2026-05-20T10:00:00.001Z", "2026-05-20T10:00:00.000Z")).toBe(1);
    expect(compareTimelineTimestamps("2026-05-20T10:00:00.000Z", "2026-05-20T10:00:00.000Z")).toBe(0);
  });

  test("falls back to lexicographic compare for unparseable values", () => {
    expect(compareTimelineTimestamps("not-a-date", "still-not-a-date")).toBeLessThan(0);
    expect(compareTimelineTimestamps(undefined, undefined)).toBe(0);
  });
});

describe("compareTimelineMessages", () => {
  test("uses createdAt when timestamps differ", () => {
    const earlier: SortableMessage = { id: "a", createdAt: "2026-05-20T10:00:00.000Z" };
    const later: SortableMessage = { id: "b", createdAt: "2026-05-20T10:00:00.001Z" };
    expect(compareTimelineMessages(earlier, later)).toBe(-1);
    expect(compareTimelineMessages(later, earlier)).toBe(1);
  });

  test("falls through to stanzaId when createdAt is equal", () => {
    const a: SortableMessage = { id: "x", stanzaId: "stanza-a", createdAt: "2026-05-20T10:00:00.000Z" };
    const b: SortableMessage = { id: "y", stanzaId: "stanza-b", createdAt: "2026-05-20T10:00:00.000Z" };
    expect(compareTimelineMessages(a, b)).toBeLessThan(0);
    expect(compareTimelineMessages(b, a)).toBeGreaterThan(0);
  });

  test("falls through to id when createdAt and stanzaId are equal", () => {
    const a: SortableMessage = { id: "id-a", createdAt: "2026-05-20T10:00:00.000Z" };
    const b: SortableMessage = { id: "id-b", createdAt: "2026-05-20T10:00:00.000Z" };
    expect(compareTimelineMessages(a, b)).toBeLessThan(0);
    expect(compareTimelineMessages(b, a)).toBeGreaterThan(0);
  });

  test("never returns 0 for two distinct messages", () => {
    const messages: SortableMessage[] = [
      { id: "a", createdAt: "2026-05-20T10:00:00.000Z", stanzaId: "s1" },
      { id: "b", createdAt: "2026-05-20T10:00:00.000Z", stanzaId: "s2" },
      { id: "c", createdAt: "2026-05-20T10:00:00.000Z" },
      { id: "d", createdAt: "2026-05-20T10:00:00.000Z" },
      { id: "e", createdAt: "2026-05-20T10:00:00.001Z" },
    ];
    for (let i = 0; i < messages.length; i++) {
      for (let j = i + 1; j < messages.length; j++) {
        expect(compareTimelineMessages(messages[i]!, messages[j]!)).not.toBe(0);
      }
    }
  });

  test("pending self-sends (queued, sending) always sort after delivered/peer messages", () => {
    const delivered: SortableMessage = {
      id: "delivered",
      createdAt: "2026-05-20T10:00:00.000Z",
      deliveryStatus: "delivered",
    };
    const peer: SortableMessage = { id: "peer", createdAt: "2026-05-20T10:00:00.000Z" };
    const queued: SortableMessage = {
      id: "queued",
      // Pending row carries client wall-clock that is *earlier* than the
      // already-delivered server message — without the pending rule it
      // would mis-sort to the front.
      createdAt: "2026-05-20T09:59:59.000Z",
      deliveryStatus: "queued",
    };
    const sending: SortableMessage = {
      id: "sending",
      createdAt: "2026-05-20T09:00:00.000Z",
      deliveryStatus: "sending",
    };
    expect(compareTimelineMessages(queued, delivered)).toBeGreaterThan(0);
    expect(compareTimelineMessages(queued, peer)).toBeGreaterThan(0);
    expect(compareTimelineMessages(sending, delivered)).toBeGreaterThan(0);
    // Among pending rows, normal createdAt ordering applies.
    expect(compareTimelineMessages(sending, queued)).toBeLessThan(0);
  });

  test("failed self-sends are NOT in the pending bucket — they keep their attempt position", () => {
    const failed: SortableMessage = {
      id: "failed",
      createdAt: "2026-05-20T08:00:00.000Z",
      deliveryStatus: "failed",
    };
    const delivered: SortableMessage = {
      id: "delivered",
      createdAt: "2026-05-20T10:00:00.000Z",
      deliveryStatus: "delivered",
    };
    // Failed precedes delivered by createdAt — keep that order so the
    // user retries the failed send in the context where it happened.
    expect(compareTimelineMessages(failed, delivered)).toBeLessThan(0);
  });

  test("sort is stable across permutations (final order is identical)", () => {
    const canonical: SortableMessage[] = [
      { id: "a", createdAt: "2026-05-20T10:00:00.000Z", stanzaId: "s-001" },
      { id: "b", createdAt: "2026-05-20T10:00:00.000Z", stanzaId: "s-002" },
      { id: "c", createdAt: "2026-05-20T10:00:00.001Z" },
      { id: "d", createdAt: "2026-05-20T10:00:00.001Z" },
      { id: "e", createdAt: "2026-05-20T10:00:00.002Z", deliveryStatus: "delivered" },
      { id: "f", createdAt: "2026-05-20T09:59:00.000Z", deliveryStatus: "sending" },
      { id: "g", createdAt: "2026-05-20T09:58:00.000Z", deliveryStatus: "queued" },
    ];
    const reference = [...canonical].sort(compareTimelineMessages).map((m) => m.id);
    for (let seed = 1; seed <= 25; seed++) {
      const shuffled = shuffle(canonical, seed);
      const sorted = shuffled.sort(compareTimelineMessages).map((m) => m.id);
      expect(sorted).toEqual(reference);
    }
  });
});

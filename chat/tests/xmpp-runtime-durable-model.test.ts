import { describe, expect, test } from "bun:test";
import type { PersistedQueuedDmMessage } from "../src/lib/outbound-queue-store";
import {
  checkedDurableCounterIncrement,
  checkedDurableDeadline,
  emptyAccount,
  orderKey,
  sameIdentity,
  sameLane,
} from "../src/lib/xmpp-runtime/durable-model";

describe("durable runtime model invariants", () => {
  test("checked counters and deadlines fail closed before overflow", () => {
    expect(checkedDurableCounterIncrement(
      Number.MAX_SAFE_INTEGER - 1,
      "test",
    )).toBe(Number.MAX_SAFE_INTEGER);
    expect(() => checkedDurableCounterIncrement(
      Number.MAX_SAFE_INTEGER,
      "test",
    )).toThrow("test counter exhausted");
    expect(() => checkedDurableCounterIncrement(-1, "test")).toThrow(
      "test counter exhausted",
    );

    expect(checkedDurableDeadline(100, 45_000, "lease")).toBe(45_100);
    expect(() => checkedDurableDeadline(
      Number.MAX_SAFE_INTEGER,
      1,
      "lease",
    )).toThrow("lease deadline exhausted");
  });

  test("empty accounts isolate dictionaries and retain canonical ordering", () => {
    const first = emptyAccount("first@example.com");
    const second = emptyAccount("second@example.com");
    first.outbound.message = {} as never;

    expect(second.outbound.message).toBeUndefined();
    expect(Object.getPrototypeOf(first.outbound)).toBeNull();

    const message: PersistedQueuedDmMessage = {
      kind: "dm",
      id: "message-1",
      createdAt: "2026-07-17T00:00:00.000Z",
      peerJid: "recipient@example.com",
      body: "hello",
    };
    expect(orderKey(message)).toBe(
      "2026-07-17T00:00:00.000Z\u0000message-1",
    );
  });

  test("lane and row identity equality compare every fence component", () => {
    expect(sameLane({ kind: "direct" }, { kind: "direct" })).toBe(true);
    expect(sameLane(
      { kind: "room", roomJid: "room@example.com" },
      { kind: "room", roomJid: "other@example.com" },
    )).toBe(false);

    const identity = {
      accountKey: "account@example.com",
      messageId: "message-1",
      incarnation: "incarnation-1",
      payloadDigest: "digest-1",
    };
    expect(sameIdentity(identity, { ...identity })).toBe(true);
    expect(sameIdentity(identity, {
      ...identity,
      payloadDigest: "digest-2",
    })).toBe(false);
  });
});

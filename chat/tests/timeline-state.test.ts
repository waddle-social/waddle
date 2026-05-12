import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { applyDeliveryEventById, latestRemoteMessageIdFor } from "../src/lib/timeline-state";

function makeMessage(id: string, overrides: Partial<TimelineMessage> = {}): TimelineMessage {
  return {
    id,
    body: "msg",
    nick: "alice",
    timestamp: 0,
    isSelf: true,
    deliveryStatus: "queued",
    ...overrides,
  } as TimelineMessage;
}

describe("latestRemoteMessageIdFor", () => {
  test("returns null for an empty timeline", () => {
    expect(latestRemoteMessageIdFor([])).toBeNull();
  });

  test("returns the most recent non-self, non-retracted message id", () => {
    const timeline = [
      makeMessage("a", { isSelf: false }),
      makeMessage("b", { isSelf: false }),
      makeMessage("c", { isSelf: true }),
    ];
    expect(latestRemoteMessageIdFor(timeline)).toBe("b");
  });

  test("skips trailing self-sends", () => {
    const timeline = [
      makeMessage("a", { isSelf: false }),
      makeMessage("b", { isSelf: true }),
      makeMessage("c", { isSelf: true }),
    ];
    expect(latestRemoteMessageIdFor(timeline)).toBe("a");
  });

  test("skips retracted messages", () => {
    const timeline = [
      makeMessage("a", { isSelf: false }),
      makeMessage("b", { isSelf: false, isRetracted: true } as Partial<TimelineMessage>),
    ];
    expect(latestRemoteMessageIdFor(timeline)).toBe("a");
  });

  test("returns null when timeline contains only self/retracted messages", () => {
    const timeline = [
      makeMessage("a", { isSelf: true }),
      makeMessage("b", { isSelf: false, isRetracted: true } as Partial<TimelineMessage>),
    ];
    expect(latestRemoteMessageIdFor(timeline)).toBeNull();
  });
});

describe("applyDeliveryEventById", () => {
  test("updates the matching self-message's deliveryStatus", () => {
    const timeline = [makeMessage("a"), makeMessage("b", { deliveryStatus: "sending" })];
    const next = applyDeliveryEventById(timeline, "b", "delivered");
    expect(next.find((m) => m.id === "b")?.deliveryStatus).toBe("delivered");
  });

  test("leaves non-matching messages untouched", () => {
    const timeline = [makeMessage("a"), makeMessage("b", { deliveryStatus: "sending" })];
    const next = applyDeliveryEventById(timeline, "b", "delivered");
    expect(next.find((m) => m.id === "a")).toBe(timeline[0]!);
  });

  test("never mutates messages that aren't self", () => {
    const timeline = [makeMessage("a", { isSelf: false, deliveryStatus: "sending" })];
    const next = applyDeliveryEventById(timeline, "a", "delivered");
    expect(next[0]?.deliveryStatus).toBe("sending");
  });

  test("returns the same array reference when nothing changed (id miss)", () => {
    const timeline = [makeMessage("a"), makeMessage("b")];
    expect(applyDeliveryEventById(timeline, "missing", "delivered")).toBe(timeline);
  });

  test("returns the same array reference when the transition is a no-op (terminal)", () => {
    const timeline = [makeMessage("a", { deliveryStatus: "delivered" })];
    // delivered → failed is a no-op (no-downgrade rule from applyDeliveryEvent).
    // Helper should preserve reference identity to let Vue skip a re-render.
    expect(applyDeliveryEventById(timeline, "a", "failed")).toBe(timeline);
  });

  test("empty timeline returns empty", () => {
    expect(applyDeliveryEventById([], "any", "delivered")).toEqual([]);
  });

  test("matches via wireIds when the primary id was reconciled by XEP-0359", () => {
    // After a self-echo, the message's primary id swaps to the
    // server-assigned stanza-id while the original client id moves into
    // wireIds. A subsequent XEP-0198 SM ack referencing the original client
    // id must still find the message.
    const timeline = [
      makeMessage("server-stanza-id", {
        deliveryStatus: "sending",
        wireIds: ["original-client-id"],
      } as Partial<TimelineMessage>),
    ];
    const next = applyDeliveryEventById(timeline, "original-client-id", "delivered");
    expect(next.find((m) => m.id === "server-stanza-id")?.deliveryStatus).toBe("delivered");
  });

  test("preserves all other fields on the updated message", () => {
    const original = makeMessage("a", { deliveryStatus: "sending", body: "hello", nick: "bob" });
    const next = applyDeliveryEventById([original], "a", "delivered");
    const updated = next.find((m) => m.id === "a")!;
    expect(updated.deliveryStatus).toBe("delivered");
    expect(updated.body).toBe("hello");
    expect(updated.nick).toBe("bob");
    // updated must be a new object reference (Vue reactivity needs the swap)
    expect(updated).not.toBe(original);
  });
});

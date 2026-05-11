import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { applyDeliveryEventById } from "../src/lib/timeline-state";

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

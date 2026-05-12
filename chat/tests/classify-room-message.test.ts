import { describe, expect, test } from "bun:test";
import { classifyRoomMessage } from "../src/lib/xmpp/classify-room-message";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";

// Pure-helper tests for the typed-dispatch classifier that replaces the
// inline "switch on msg.replacesId / msg.retractsId" sniffing inside the
// channel-side message handler. The classifier inspects ONLY the inbound
// stanza shape — the orchestrator handles cross-room filtering and
// notification routing separately.

function makeLive(overrides: Partial<LiveRoomMessage> = {}): LiveRoomMessage {
  return {
    type: "message",
    roomJid: "room@muc.example.com",
    fromJid: "room@muc.example.com/bob",
    nick: "bob",
    body: "hello",
    timestamp: Date.now(),
    isSelf: false,
    ...overrides,
  } as unknown as LiveRoomMessage;
}

describe("classifyRoomMessage", () => {
  test("non-message stanza → ignore", () => {
    const result = classifyRoomMessage(makeLive({ type: "subject" as LiveRoomMessage["type"] }));
    expect(result.kind).toBe("ignore");
  });

  test("retraction wins over body", () => {
    const result = classifyRoomMessage(makeLive({
      retractsId: "target-1",
      retractionId: "retract-1",
      body: "",
    }));
    expect(result.kind).toBe("retraction");
    if (result.kind === "retraction") {
      expect(result.retractsId).toBe("target-1");
      expect(result.retractionId).toBe("retract-1");
    }
  });

  test("retraction with moderationTargetId surfaces it", () => {
    const result = classifyRoomMessage(makeLive({
      retractsId: "target-1",
      moderationTargetId: "target-1",
      authorRealJid: "mod@example.com/full",
    }));
    expect(result.kind).toBe("retraction");
    if (result.kind === "retraction") {
      expect(result.moderationTargetId).toBe("target-1");
    }
  });

  test("correction wins over plain body when replacesId is set", () => {
    const result = classifyRoomMessage(makeLive({
      replacesId: "orig-1",
      body: "edited",
    }));
    expect(result.kind).toBe("correction");
    if (result.kind === "correction") {
      expect(result.replacesId).toBe("orig-1");
      expect(result.body).toBe("edited");
    }
  });

  test("correction carries markup / references / extensionAnnotations through", () => {
    const result = classifyRoomMessage(makeLive({
      replacesId: "orig-1",
      body: "edited",
      markup: [{ kind: "bold", start: 0, end: 2 } as never],
      references: [{ kind: "mention", uri: "xmpp:bob@example.com" } as never],
      extensionAnnotations: [{ annotationId: "a1" } as never],
      extensionBodyFallback: true,
    } as Partial<LiveRoomMessage>));
    expect(result.kind).toBe("correction");
    if (result.kind === "correction") {
      expect(result.markup?.length).toBe(1);
      expect(result.references?.length).toBe(1);
      expect(result.extensionAnnotations?.length).toBe(1);
      expect(result.extensionBodyFallback).toBe(true);
    }
  });

  test("retraction wins when both retractsId and replacesId are present", () => {
    // Defensive: a malformed stanza setting both should retract; deleting
    // wins over editing.
    const result = classifyRoomMessage(makeLive({
      retractsId: "target-1",
      replacesId: "orig-1",
    }));
    expect(result.kind).toBe("retraction");
  });

  test("plain message (no retract, no correction) → live", () => {
    const result = classifyRoomMessage(makeLive({ body: "hello" }));
    expect(result.kind).toBe("live");
    if (result.kind === "live") {
      expect(result.raw.body).toBe("hello");
    }
  });

  test("empty body + no retract/correction → live (consumer decides what to do with empty body)", () => {
    // The pre-extraction code already passed empty-body messages through
    // to mergeLiveMessage; the classifier keeps that contract.
    const result = classifyRoomMessage(makeLive({ body: "" }));
    expect(result.kind).toBe("live");
  });
});

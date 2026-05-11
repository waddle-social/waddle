import { describe, expect, it, beforeEach } from "bun:test";
import {
  $pinnedMessageBodies,
  cachePinnedMessageBody,
  cachePinnedMessageBodies,
  evictPinnedMessageBody,
  resetPinnedMessageBodies,
  pinnedMessageBodiesEpoch,
} from "@/stores/pinned-message-bodies";
import type { TimelineMessage } from "@/lib/chat-ui";

function makeMessage(id: string, body = "hello"): TimelineMessage {
  return {
    id,
    author: "alice",
    body,
    createdAt: "2026-05-11T12:00:00Z",
    isSelf: false,
  };
}

describe("$pinnedMessageBodies", () => {
  beforeEach(() => {
    resetPinnedMessageBodies();
  });

  it("starts empty", () => {
    expect($pinnedMessageBodies.get().size).toBe(0);
  });

  it("caches a body keyed by (room, stanzaId)", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    const room = $pinnedMessageBodies.get().get("room@x");
    expect(room?.get("sid-A")?.id).toBe("m1");
  });

  it("caches multiple bodies in one call", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBodies(
      "room@x",
      [
        { stanzaId: "sid-A", message: makeMessage("m1") },
        { stanzaId: "sid-B", message: makeMessage("m2") },
      ],
      epoch,
    );
    expect($pinnedMessageBodies.get().get("room@x")?.size).toBe(2);
  });

  it("evicts an entry on unpin", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    evictPinnedMessageBody("room@x", "sid-A");
    expect($pinnedMessageBodies.get().get("room@x")?.has("sid-A")).toBeFalsy();
    // Cleanup: the outer room key is removed once its inner map is empty.
    expect($pinnedMessageBodies.get().has("room@x")).toBe(false);
  });

  it("evicting one entry leaves the room with remaining entries intact", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("mA"), epoch);
    cachePinnedMessageBody("room@x", "sid-B", makeMessage("mB"), epoch);
    evictPinnedMessageBody("room@x", "sid-A");
    expect($pinnedMessageBodies.get().has("room@x")).toBe(true);
    expect($pinnedMessageBodies.get().get("room@x")?.has("sid-A")).toBe(false);
    expect($pinnedMessageBodies.get().get("room@x")?.get("sid-B")?.id).toBe("mB");
  });

  it("drops late writes after epoch bump", () => {
    const epoch = pinnedMessageBodiesEpoch();
    resetPinnedMessageBodies();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    expect($pinnedMessageBodies.get().get("room@x")).toBeUndefined();
  });

  it("reset clears all rooms", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    cachePinnedMessageBody("room@y", "sid-B", makeMessage("m2"), epoch);
    resetPinnedMessageBodies();
    expect($pinnedMessageBodies.get().size).toBe(0);
  });

  it("cachePinnedMessageBodies with empty entries is a no-op", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBodies("room@x", [], epoch);
    expect($pinnedMessageBodies.get().size).toBe(0);
  });

  it("evicting after reset is a no-op", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    resetPinnedMessageBodies();
    expect(() => evictPinnedMessageBody("room@x", "sid-A")).not.toThrow();
    expect($pinnedMessageBodies.get().size).toBe(0);
  });

  it("cachePinnedMessageBodies with duplicate stanza-ids keeps the last write", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBodies(
      "room@x",
      [
        { stanzaId: "A", message: makeMessage("m1") },
        { stanzaId: "A", message: makeMessage("m2") },
      ],
      epoch,
    );
    const room = $pinnedMessageBodies.get().get("room@x");
    expect(room?.size).toBe(1);
    expect(room?.get("A")?.id).toBe("m2");
  });
});

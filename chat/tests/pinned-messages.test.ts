import { describe, expect, it, beforeEach } from "bun:test";
import { applyPinEvent, hydratePinnedRoom, resetPinnedRooms } from "@/stores/pinned-messages";
import {
  $pinnedMessageBodies,
  cachePinnedMessageBody,
  pinnedMessageBodiesEpoch,
  resetPinnedMessageBodies,
} from "@/stores/pinned-message-bodies";
import type { TimelineMessage } from "@/lib/chat-ui";

const fakeMessage: TimelineMessage = {
  id: "sid-A",
  author: "alice",
  body: "live",
  createdAt: "2026-05-11T12:00:00Z",
  isSelf: false,
};

describe("applyPinEvent(unpin) eviction", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("evicts cached body when entry is unpinned", () => {
    hydratePinnedRoom("room@x", [
      {
        target_stanza_id: "sid-A",
        pinner_jid: "admin@example.com",
        pinned_at: "2026-05-11T12:00:00Z",
        preview: {
          author_jid: "alice@example.com",
          text: "",
          message_timestamp: "2026-05-11T11:50:00Z",
        },
      },
    ]);
    cachePinnedMessageBody("room@x", "sid-A", fakeMessage, pinnedMessageBodiesEpoch());
    applyPinEvent("room@x", { action: "unpinned", target_stanza_id: "sid-A" });
    expect($pinnedMessageBodies.get().get("room@x")?.has("sid-A")).toBeFalsy();
  });
});

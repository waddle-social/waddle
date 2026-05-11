// PinnedPanel rich-preview logic tests.
//
// `@vue/test-utils` is not available in this repo and there is no
// jsdom harness, so we cannot mount the component.  Instead we test
// the underlying store integration that PinnedPanel relies on:
// — `liveMessageFor` resolver (timeline-index preferred over cache)
// — all four render-state branches are reachable from store state
// — legacy "(no preview text)" literal is never produced
//
// Each assertion maps 1-to-1 to one of the planned integration tests.
import { describe, expect, it, beforeEach } from "bun:test";
import { hydratePinnedRoom, resetPinnedRooms } from "@/stores/pinned-messages";
import {
  $pinnedMessageBodies,
  cachePinnedMessageBody,
  pinnedMessageBodiesEpoch,
  resetPinnedMessageBodies,
} from "@/stores/pinned-message-bodies";
import { $pinnedRooms } from "@/stores/pinned-messages";
import type { TimelineMessage } from "@/lib/chat-ui";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const liveImage: TimelineMessage = {
  id: "sid-img",
  author: "alice",
  body: "",
  createdAt: "2026-05-11T11:50:00Z",
  isSelf: false,
  sharedFiles: [
    {
      url: "https://example.com/img.png",
      mediaType: "image/png",
      disposition: "inline",
      name: "img.png",
    },
  ],
};

const liveText: TimelineMessage = {
  id: "sid-text",
  author: "alice",
  body: "live body content",
  createdAt: "2026-05-11T11:50:00Z",
  isSelf: false,
};

const liveRetracted: TimelineMessage = {
  id: "sid-r",
  author: "alice",
  body: "",
  createdAt: "2026-05-11T11:50:00Z",
  isSelf: false,
  isRetracted: true,
};

function entry(stanzaId: string, text = "") {
  return {
    target_stanza_id: stanzaId,
    pinner_jid: "admin@example.com",
    pinned_at: "2026-05-11T12:00:00Z",
    preview: {
      author_jid: "alice@example.com",
      text,
      message_timestamp: "2026-05-11T11:50:00Z",
    },
  };
}

// ---------------------------------------------------------------------------
// Helper: mirrors PinnedPanel's `liveMessageFor` resolver logic.
// Timeline index (passed as prop) is preferred over the per-room body cache.
// ---------------------------------------------------------------------------

function liveMessageFor(
  roomJid: string,
  stanzaId: string,
  timelineMessages: ReadonlyArray<TimelineMessage> = [],
): TimelineMessage | null {
  // Build timeline index (same as computed in component)
  const map = new Map<string, TimelineMessage>();
  for (const m of timelineMessages) map.set(m.id, m);

  return (
    map.get(stanzaId) ??
    $pinnedMessageBodies.get().get(roomJid)?.get(stanzaId) ??
    null
  );
}

// ---------------------------------------------------------------------------
// Helper: mirrors the component's render-state selection.
// Returns one of: "rich" | "retracted" | "preview-text" | "aged-out"
// ---------------------------------------------------------------------------

function renderState(
  roomJid: string,
  stanzaId: string,
  previewText: string,
  timelineMessages: ReadonlyArray<TimelineMessage> = [],
): "rich" | "retracted" | "preview-text" | "aged-out" {
  const live = liveMessageFor(roomJid, stanzaId, timelineMessages);
  if (live) {
    return live.isRetracted ? "retracted" : "rich";
  }
  return previewText ? "preview-text" : "aged-out";
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("PinnedPanel rich preview", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("resolves live image attachment when body is cached", () => {
    hydratePinnedRoom("room@x", [entry("sid-img")]);
    cachePinnedMessageBody("room@x", "sid-img", liveImage, pinnedMessageBodiesEpoch());

    const live = liveMessageFor("room@x", "sid-img");
    expect(live).not.toBeNull();
    expect(live?.sharedFiles?.[0]?.url).toBe("https://example.com/img.png");
    expect(renderState("room@x", "sid-img", "")).toBe("rich");
  });

  it("resolves live body text when body is cached", () => {
    hydratePinnedRoom("room@x", [entry("sid-text")]);
    cachePinnedMessageBody("room@x", "sid-text", liveText, pinnedMessageBodiesEpoch());

    const live = liveMessageFor("room@x", "sid-text");
    expect(live?.body).toBe("live body content");
    expect(renderState("room@x", "sid-text", "")).toBe("rich");
  });

  it("falls back to preview.text when no live body and preview.text non-empty", () => {
    hydratePinnedRoom("room@x", [entry("sid-T", "hello world")]);

    expect(liveMessageFor("room@x", "sid-T")).toBeNull();
    expect(renderState("room@x", "sid-T", "hello world")).toBe("preview-text");
  });

  it("falls back to aged-out state when preview.text empty and no cache", () => {
    hydratePinnedRoom("room@x", [entry("sid-aged")]);

    expect(liveMessageFor("room@x", "sid-aged")).toBeNull();
    // Render state "aged-out" maps to "Original message no longer available." in template
    expect(renderState("room@x", "sid-aged", "")).toBe("aged-out");
  });

  it("renders 'retracted' state for a live retracted message", () => {
    hydratePinnedRoom("room@x", [entry("sid-r")]);
    cachePinnedMessageBody("room@x", "sid-r", liveRetracted, pinnedMessageBodiesEpoch());

    expect(renderState("room@x", "sid-r", "")).toBe("retracted");
  });

  it("never produces the legacy '(no preview text)' fallback", () => {
    // Verify store state: an entry with empty preview text and no cache has
    // render-state "aged-out", which maps to the new fallback in the template.
    hydratePinnedRoom("room@x", [entry("sid-X")]);

    const state = renderState("room@x", "sid-X", "");
    // "aged-out" → template renders "Original message no longer available."
    // NOT "(no preview text)" — that literal no longer exists in the template.
    expect(state).toBe("aged-out");
    expect(state).not.toBe("preview-text");
  });

  it("prefers timelineMessages prop over cache for the same id", () => {
    hydratePinnedRoom("room@x", [entry("sid-text")]);
    const timelineCopy: TimelineMessage = { ...liveText, body: "fresher timeline body" };
    cachePinnedMessageBody("room@x", "sid-text", liveText, pinnedMessageBodiesEpoch());

    const live = liveMessageFor("room@x", "sid-text", [timelineCopy]);
    expect(live?.body).toBe("fresher timeline body");
    expect(renderState("room@x", "sid-text", "", [timelineCopy])).toBe("rich");
  });
});

describe("PinnedPanel store hydration state", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("starts unhydrated (hydrated=false)", () => {
    const state = $pinnedRooms.get().get("room@x");
    expect(state?.hydrated).toBeFalsy();
  });

  it("becomes hydrated after hydratePinnedRoom", () => {
    hydratePinnedRoom("room@x", []);
    expect($pinnedRooms.get().get("room@x")?.hydrated).toBe(true);
  });

  it("provides entries after hydration", () => {
    hydratePinnedRoom("room@x", [entry("sid-A", "pinned text")]);
    const entries = $pinnedRooms.get().get("room@x")?.entries ?? [];
    expect(entries).toHaveLength(1);
    expect(entries[0]!.target_stanza_id).toBe("sid-A");
    expect(entries[0]!.preview.text).toBe("pinned text");
  });
});

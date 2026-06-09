import { describe, expect, test } from "bun:test";
import { buildDmCallStartedAnchor, dmCallAnchorId } from "../src/lib/calls/dm-call-anchor";
import { isFeedTimelineMessage } from "../src/channels/timeline";
import { buildDmTimelineFromMamResults } from "../src/dms/message-timeline-state";
import type { LiveDmMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";

const base = {
  peerBareJid: "bob@waddle.test",
  sid: "sid-123",
  media: { audio: true, video: false },
  initiator: "alice@waddle.test/web",
  started: "2026-06-09T12:00:00Z",
};

describe("dm call-started anchor", () => {
  test("builds a feed-visible dm call anchor keyed by the call sid", () => {
    const anchor = buildDmCallStartedAnchor(base, "alice@waddle.test/desktop");

    expect(anchor.id).toBe(dmCallAnchorId("sid-123"));
    expect(anchor.threadId).toBe("sid-123");
    expect(anchor.callThread).toEqual({
      kind: "dm",
      sid: "sid-123",
      media: ["audio"],
      initiator: "alice@waddle.test/web",
      started: "2026-06-09T12:00:00Z",
    });
    expect(anchor.createdAtSource).toBe("fallback");
    // The initiator is the local user (same bare JID, different resource).
    expect(anchor.isSelf).toBe(true);
    // A call anchor is always feed-visible despite carrying a thread id.
    expect(isFeedTimelineMessage(anchor)).toBe(true);
  });

  test("marks the anchor as remote when the peer is the initiator", () => {
    const anchor = buildDmCallStartedAnchor(
      { ...base, initiator: "bob@waddle.test/phone" },
      "alice@waddle.test/desktop",
    );

    expect(anchor.isSelf).toBe(false);
  });

  test("includes video when the call carries it", () => {
    const anchor = buildDmCallStartedAnchor(
      { ...base, media: { audio: true, video: true } },
      "alice@waddle.test/desktop",
    );

    expect(anchor.callThread?.media).toEqual(["audio", "video"]);
  });

  test("a MAM-replayed anchor dedups onto the synthesized live card by sid", () => {
    const session = { jid: "alice@waddle.test/web", username: "alice" } as WaddleSession;
    const synth = buildDmCallStartedAnchor(base, session.jid);

    // The archived `<proceed/>` row carries a distinct primary id but is
    // aliased by the call sid, so a same-session backfill must merge — not
    // append a second "started a call" card.
    const mamAnchor: LiveDmMessage = {
      id: "proceed-stanza-id",
      peerJid: "bob@waddle.test",
      fromJid: "bob@waddle.test/phone",
      nick: "bob",
      body: "",
      createdAt: "2026-06-09T12:00:05Z",
      createdAtSource: "archive",
      type: "message",
      threadId: base.sid,
      wireIds: [dmCallAnchorId(base.sid)],
      callThread: {
        kind: "dm",
        sid: base.sid,
        media: ["audio"],
        initiator: base.initiator,
        started: base.started,
      },
    };

    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [mamAnchor],
      existing: [synth],
    });

    expect(timeline.filter((m) => m.callThread)).toHaveLength(1);
  });
});

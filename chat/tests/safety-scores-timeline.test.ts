// XEP-0422 score result resolution in the web timeline.
import { afterEach, describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { ChannelPendingUpdates } from "../src/channels/pending-updates";
import { useChannelMamPaging } from "../src/channels/mam-paging";
import { useChannelLiveMerge } from "../src/channels/live-merge";
import { buildChannelTimelineFromMamResults } from "../src/channels/message-timeline-state";
import { applySafetyScoresFastening, safetyScoresTargetIndex } from "../src/lib/safety-scores/apply";
import type { SafetyScores, SafetyScoresFastening } from "../src/lib/safety-scores/types";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { WaddleSession } from "../src/lib/server-auth";
import type { BrowserXmppClient, LiveRoomMessage } from "../src/lib/xmpp-client";
import { __setFaroForTesting } from "../src/lib/telemetry";

afterEach(() => __setFaroForTesting(null));
const ROOM = "general@conference.example.org";
const AUTHOR = `${ROOM}/bob`;

const session: WaddleSession = {
  session_id: "session-1", user_id: "alice-id", username: "alice", avatar_url: null,
  xmpp_localpart: "alice", jid: "alice@example.org/web",
  xmpp_websocket_url: "wss://example.org/ws", is_expired: false, expires_at: null,
};

function scores(value: number): SafetyScores {
  return { modelVersion: "jev-1", scores: [
    { category: "is_question", probability: value, taxonomyVersion: "q-v1" },
  ] };
}

function result(value: number, overrides: Partial<SafetyScoresFastening> = {}): SafetyScoresFastening {
  return {
    targetOriginId: "origin-1", targetStanzaId: "room-1", targetStanzaBy: ROOM,
    sourceRevisionId: "room-1", scores: scores(value), ...overrides,
  };
}

function row(overrides: Partial<TimelineMessage> = {}): TimelineMessage {
  return {
    id: "room-1", stanzaId: "room-1", stanzaIdBy: ROOM, originId: "origin-1",
    wireIds: ["origin-1"], replyableId: "room-1",
    author: "bob", authorJid: AUTHOR, authorOccupantJid: AUTHOR,
    body: "original", createdAt: "2026-09-25T09:59:00Z",
    createdAtSource: "archive", isSelf: false, ...overrides,
  };
}

function message(overrides: Partial<LiveRoomMessage> = {}): LiveRoomMessage {
  return {
    id: "room-1", archiveId: "room-1", stanzaId: "room-1", stanzaIdBy: ROOM,
    originId: "origin-1", wireIds: ["origin-1"], replyableId: "room-1",
    fromJid: AUTHOR, roomJid: ROOM, nick: "bob", body: "original",
    createdAt: "2026-09-25T09:59:00Z", createdAtSource: "archive", type: "message",
    ...overrides,
  };
}

function scoreMessage(id: string, fastening: SafetyScoresFastening, at = "2026-09-25T10:00:00Z"): LiveRoomMessage {
  return message({
    id, archiveId: id, fromJid: ROOM, nick: "unknown", body: "",
    createdAt: at, safetyScoresFastening: fastening,
  });
}

describe("safety score identity", () => {
  test("requires origin and room stanza identity on the same row", () => {
    const rows = [
      row({ id: "wrong", stanzaId: "wrong", originId: "origin-1" }),
      row({ id: "room-1", stanzaId: "room-1", originId: "other-origin" }),
    ];
    expect(safetyScoresTargetIndex(rows, result(0.5))).toBe(-1);
    expect(safetyScoresTargetIndex([row()], result(0.5))).toBe(0);
    expect(safetyScoresTargetIndex([row(), row({ id: "duplicate" })], result(0.5))).toBe(-1);
  });

  test("rejects a result for another room or body revision", () => {
    expect(safetyScoresTargetIndex([row()], result(0.5, { targetStanzaBy: "other@conference.example.org" }))).toBe(-1);
    expect(safetyScoresTargetIndex([row({ sourceRevisionId: "edit-2" })], result(0.5))).toBe(-1);
    expect(safetyScoresTargetIndex([row({ isRetracted: true })], result(0.5))).toBe(-1);
  });

  test("newer result wins against an old archive replay", () => {
    const newer = applySafetyScoresFastening([row()], result(0.9), "2026-09-25T10:05:00Z");
    expect(newer?.[0]?.safetyScores).toEqual(scores(0.9));
    expect(applySafetyScoresFastening(newer!, result(0.1), "2026-09-25T10:00:00Z")).toBeNull();
  });
});

describe("channel score handling", () => {
  function harness(initial: TimelineMessage[]) {
    const messages = ref<TimelineMessage[]>(initial);
    const pendingUpdates = new ChannelPendingUpdates();
    const live = useChannelLiveMerge({
      pendingUpdates,
      session: ref(session), messages, activeChannelId: ref("general"),
      pendingEchoClientIds: new Set<string>(),
      scrollToPinnedEdgeAndPin: mock(async () => true),
      persistLastSeen: mock(() => {}),
    });
    const mam = (mamResults: LiveRoomMessage[]) => {
      messages.value = buildChannelTimelineFromMamResults({
        session, channelIsForum: false, mamResults, existing: messages.value, pendingUpdates,
      });
    };
    return { messages, live, mam, pendingUpdates };
  }

  test("a live result annotates without inserting", () => {
    const h = harness([row()]);
    expect(h.live.handleRoomMessage(scoreMessage("score-1", result(0.9))).kind).toBe("ignore");
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.9));
  });

  test("correction clears the score and blocks old revision replay", () => {
    const h = harness([row()]);
    h.live.handleRoomMessage(scoreMessage("score-1", result(0.9)));
    h.live.handleRoomMessage(message({
      id: "edit-2", stanzaId: "edit-2", originId: "edit-origin",
      replacesId: "origin-1", body: "edited",
    }));
    expect(h.messages.value[0]?.safetyScores).toBeUndefined();
    expect(h.messages.value[0]?.sourceRevisionId).toBe("edit-2");
    h.live.handleRoomMessage(scoreMessage("old-score", result(0.9)));
    expect(h.messages.value[0]?.safetyScores).toBeUndefined();
    h.live.handleRoomMessage(scoreMessage("new-score", result(0.4, { sourceRevisionId: "edit-2" })));
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.4));
  });

  test("live score arriving before its correction waits for that revision", () => {
    const h = harness([row()]);
    h.live.handleRoomMessage(scoreMessage("new-score", result(0.4, { sourceRevisionId: "edit-2" })));
    expect(h.messages.value[0]?.safetyScores).toBeUndefined();
    h.live.handleRoomMessage(message({
      id: "edit-2", stanzaId: "edit-2", originId: "edit-origin",
      replacesId: "origin-1", body: "edited",
    }));
    expect(h.messages.value[0]?.sourceRevisionId).toBe("edit-2");
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.4));
  });

  test("live score arriving before its source waits for the source", () => {
    const h = harness([]);
    h.live.handleRoomMessage(scoreMessage("score-1", result(0.9)));
    h.live.handleRoomMessage(message());
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.9));
  });

  test("three reverse MAM pages retain score and correction until the source arrives", () => {
    const h = harness([]);
    h.mam([scoreMessage("new-score", result(0.4, { sourceRevisionId: "edit-2" }))]);
    h.mam([message({ id: "edit-2", stanzaId: "edit-2", originId: "edit-origin",
      replacesId: "origin-1", body: "edited", createdAt: "2026-09-25T10:01:00Z" })]);
    expect(h.messages.value).toHaveLength(0);
    h.mam([message()]);
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.body).toBe("edited");
    expect(h.messages.value[0]?.sourceRevisionId).toBe("edit-2");
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.4));
  });

  test("live score waiting for a source is drained by MAM", () => {
    const h = harness([]);
    h.live.handleRoomMessage(scoreMessage("score-1", result(0.9)));
    h.mam([message()]);
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.9));
  });

  test("MAM score waiting for a source is drained by live delivery", () => {
    const h = harness([]);
    h.mam([scoreMessage("score-1", result(0.9))]);
    h.live.handleRoomMessage(message());
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.9));
  });

  test("same correction replay preserves the accepted revision score", () => {
    const h = harness([row()]);
    const correction = message({ id: "edit-2", stanzaId: "edit-2", originId: "edit-origin",
      replacesId: "origin-1", body: "edited", createdAt: "2026-09-25T10:01:00Z" });
    h.live.handleRoomMessage(correction);
    h.live.handleRoomMessage(scoreMessage("new-score", result(0.4, { sourceRevisionId: "edit-2" })));
    h.mam([correction]);
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.4));
    h.live.handleRoomMessage(correction);
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.4));
  });

  test("older MAM corrections cannot replace the scored revision, including timestamp ties", () => {
    for (const olderAt of ["2026-09-25T10:00:00Z", "2026-09-25T10:01:00Z"]) {
      const h = harness([]);
      h.mam([scoreMessage("new-score", result(0.4, { sourceRevisionId: "edit-2" }))]);
      h.mam([message({ id: "edit-2", stanzaId: "edit-2", replacesId: "origin-1",
        body: "new edit", createdAt: "2026-09-25T10:01:00Z" })]);
      h.mam([message({ id: "edit-1", stanzaId: "edit-1", replacesId: "origin-1",
        body: "old edit", createdAt: olderAt }), message()]);
      expect(h.messages.value[0]?.body).toBe("new edit");
      expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.4));
      h.mam([message({ id: "edit-1", stanzaId: "edit-1", replacesId: "origin-1",
        body: "old edit", createdAt: olderAt })]);
      expect(h.messages.value[0]?.body).toBe("new edit");
      expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.4));
    }
  });

  test("a pending score cannot annotate an archived tombstone or another room", () => {
    const h = harness([]);
    h.live.handleRoomMessage(scoreMessage("score-1", result(0.9)));
    h.mam([message({ isRetracted: true })]);
    expect(h.messages.value[0]?.safetyScores).toBeUndefined();
    h.messages.value = [];
    h.mam([message({ roomJid: "other@conference.example.org", stanzaIdBy: "other@conference.example.org" })]);
    expect(h.messages.value[0]?.safetyScores).toBeUndefined();
  });

  test("pending scores are bounded to the latest 100", () => {
    const h = harness([]);
    h.live.handleRoomMessage(scoreMessage("score-1", result(0.9)));
    for (let i = 0; i < 100; i++) {
      h.live.handleRoomMessage(scoreMessage(`score-${i + 2}`, result(0.4, { targetStanzaId: `other-${i}` })));
    }
    h.mam([message()]);
    expect(h.messages.value[0]?.safetyScores).toBeUndefined();
  });

  test("paging reset and a fresh room load clear shared pending state", async () => {
    const h = harness([]);
    const paging = useChannelMamPaging({
      pendingUpdates: h.pendingUpdates,
      session: ref(session), messages: h.messages,
      xmppClient: ref({ queryMamPage: async () => ({ messages: [], complete: true }) } as unknown as BrowserXmppClient),
      activeSpaceId: ref("space"), activeChannelId: ref("general"),
      currentChannel: ref(null), firstUnseenId: ref(null), timelineEl: ref(null),
      scrollDirection: ref("bottom"), pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""), clearActionError: () => {}, normalizeError: String,
      pendingEchoClientIds: new Set(), appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => ROOM, isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true, persistLastSeen: () => {},
    });
    for (const reset of [() => paging.reset(), () => paging.loadMessages("space", "general")]) {
      h.messages.value = [];
      h.live.handleRoomMessage(scoreMessage("score-1", result(0.9)));
      h.live.handleRoomMessage(message({ id: "edit-2", stanzaId: "edit-2", replacesId: "origin-1", body: "edited" }));
      await reset();
      h.mam([message()]);
      expect(h.messages.value[0]?.body).toBe("original");
      expect(h.messages.value[0]?.safetyScores).toBeUndefined();
    }
  });

  test("retraction clears the visible score", () => {
    const h = harness([row()]);
    h.live.handleRoomMessage(scoreMessage("score-1", result(0.9)));
    h.live.handleRoomMessage(message({ id: "retract-1", retractsId: "room-1", body: "" }));
    expect(h.messages.value[0]?.isRetracted).toBe(true);
    expect(h.messages.value[0]?.safetyScores).toBeUndefined();
  });

  test("MAM applies only the current revision result", () => {
    const timeline = buildChannelTimelineFromMamResults({
      session, channelIsForum: false,
      mamResults: [
        message(),
        scoreMessage("old-score", result(0.9), "2026-09-25T10:00:00Z"),
        message({ id: "edit-2", stanzaId: "edit-2", originId: "edit-origin",
          replacesId: "origin-1", body: "edited", createdAt: "2026-09-25T10:01:00Z" }),
        scoreMessage("new-score", result(0.4, { sourceRevisionId: "edit-2" }), "2026-09-25T10:02:00Z"),
      ],
    });
    expect(timeline).toHaveLength(1);
    expect(timeline[0]?.safetyScores).toEqual(scores(0.4));
  });
});

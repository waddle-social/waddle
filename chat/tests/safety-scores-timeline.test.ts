// Applying the XEP-0422 `urn:waddle:safety-scores:1` fastening to the
// timeline row it targets: live (channel + DM), MAM rebuild, and the
// client-level dispatch that keeps DM fastenings off the body path.

import { afterEach, describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelLiveMerge } from "../src/channels/live-merge";
import { buildChannelTimelineFromMamResults } from "../src/channels/message-timeline-state";
import { useDmLiveMerge } from "../src/dms/live-merge";
import {
  applySafetyScoresFastening,
  findSafetyScoresTargetIndex,
} from "../src/lib/safety-scores/apply";
import type { SafetyScores, SafetyScoresFastening } from "../src/lib/safety-scores/types";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient, type LiveRoomMessage } from "../src/lib/xmpp-client";
import { __setFaroForTesting } from "../src/lib/telemetry";

afterEach(() => __setFaroForTesting(null));

const ROOM_JID = "general@conference.example.org";

const session: WaddleSession = {
  session_id: "session-1",
  user_id: "alice-id",
  username: "alice",
  avatar_url: null,
  xmpp_localpart: "alice",
  jid: "alice@example.org/web",
  xmpp_websocket_url: "wss://example.org/ws",
  is_expired: false,
  expires_at: null,
};

function scores(isQuestion: number, modelVersion = "typesafe/jev-1.13-20260917"): SafetyScores {
  return {
    modelVersion,
    scores: [
      { category: "is_question", probability: isQuestion, taxonomyVersion: "is-question-v1" },
      { category: "safety:harassment", probability: 0.02, taxonomyVersion: "safety-harassment-v1" },
    ],
  };
}

function replace(targetId: string, value: SafetyScores): SafetyScoresFastening {
  return { targetId, kind: "replace", scores: value };
}

function row(overrides: Partial<TimelineMessage> = {}): TimelineMessage {
  return {
    id: "stanza-1",
    author: "bob",
    body: "is anyone around?",
    createdAt: "2026-09-25T09:59:00Z",
    createdAtSource: "archive",
    isSelf: false,
    ...overrides,
  };
}

function roomMessage(overrides: Partial<LiveRoomMessage> = {}): LiveRoomMessage {
  return {
    id: "stanza-1",
    archiveId: "stanza-1",
    fromJid: `${ROOM_JID}/bob`,
    roomJid: ROOM_JID,
    nick: "bob",
    body: "is anyone around?",
    createdAt: "2026-09-25T09:59:00Z",
    createdAtSource: "archive",
    type: "message",
    stanzaId: "stanza-1",
    stanzaIdBy: ROOM_JID,
    reactionTargetId: "stanza-1",
    replyableId: "stanza-1",
    wireIds: ["origin-1"],
    ...overrides,
  };
}

function fasteningRecord(id: string, fastening: SafetyScoresFastening, createdAt: string): LiveRoomMessage {
  return {
    id,
    archiveId: id,
    fromJid: ROOM_JID,
    roomJid: ROOM_JID,
    nick: "unknown",
    body: "",
    createdAt,
    createdAtSource: "archive",
    type: "message",
    safetyScoresFastening: fastening,
  };
}

describe("safety-scores target resolution", () => {
  test("the room-assigned stanza-id wins over a colliding sender-chosen alias", () => {
    const timeline = [
      row({ id: "a", wireIds: ["shared-id"] }),
      row({ id: "b", stanzaId: "shared-id" }),
    ];
    expect(findSafetyScoresTargetIndex(timeline, "shared-id")).toBe(1);
  });

  test("falls back to the XEP-0359 origin-id alias (XEP-0422 §Wrapped Payloads)", () => {
    const timeline = [row({ id: "a", stanzaId: "a", wireIds: ["origin-1"] })];
    expect(findSafetyScoresTargetIndex(timeline, "origin-1")).toBe(0);
  });

  test("replace sets, a later replace overwrites, clear removes", () => {
    const initial = [row()];
    const scored = applySafetyScoresFastening(initial, replace("stanza-1", scores(0.4)));
    expect(scored?.[0]?.safetyScores).toEqual(scores(0.4));
    expect(initial[0]?.safetyScores).toBeUndefined();

    const rescored = applySafetyScoresFastening(scored!, replace("stanza-1", scores(0.9, "jev-next")));
    expect(rescored?.[0]?.safetyScores).toEqual(scores(0.9, "jev-next"));

    const cleared = applySafetyScoresFastening(rescored!, { targetId: "stanza-1", kind: "clear" });
    expect(cleared?.[0]).not.toHaveProperty("safetyScores");
    expect(cleared?.[0]?.body).toBe("is anyone around?");
  });

  test("an unknown target is a no-op", () => {
    expect(applySafetyScoresFastening([row()], replace("missing", scores(0.5)))).toBeNull();
  });
});

describe("channel live merge", () => {
  function harness(initial: TimelineMessage[]) {
    const messages = ref<TimelineMessage[]>(initial);
    const liveMerge = useChannelLiveMerge({
      session: ref(session),
      messages,
      activeChannelId: ref("general"),
      pendingEchoClientIds: new Set<string>(),
      scrollToPinnedEdgeAndPin: mock(async () => true),
      persistLastSeen: mock(() => {}),
    });
    return { messages, liveMerge };
  }

  test("a room fastening annotates its target and never becomes a row", () => {
    const h = harness([row({ stanzaId: "stanza-1" })]);
    const out = h.liveMerge.handleRoomMessage(
      fasteningRecord("fastening-1", replace("stanza-1", scores(0.92)), "2026-09-25T10:00:00Z"),
    );
    expect(out.kind).toBe("ignore");
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.safetyScores).toEqual(scores(0.92));
  });

  test("a fastening for a message outside the loaded timeline changes nothing", () => {
    const h = harness([row({ stanzaId: "stanza-1" })]);
    const before = h.messages.value;
    h.liveMerge.handleRoomMessage(
      fasteningRecord("fastening-1", replace("elsewhere", scores(0.92)), "2026-09-25T10:00:00Z"),
    );
    expect(h.messages.value).toBe(before);
  });
});

describe("channel MAM rebuild", () => {
  test("applies fastenings in archive order: replace then clear then replace", () => {
    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [
        roomMessage(),
        fasteningRecord("f-1", replace("stanza-1", scores(0.2)), "2026-09-25T10:00:00Z"),
        fasteningRecord("f-2", { targetId: "stanza-1", kind: "clear" }, "2026-09-25T10:01:00Z"),
        fasteningRecord("f-3", replace("stanza-1", scores(0.8)), "2026-09-25T10:02:00Z"),
      ],
    });
    expect(timeline.map((message) => message.id)).toEqual(["stanza-1"]);
    expect(timeline[0]?.safetyScores).toEqual(scores(0.8));
  });

  test("scores survive a rebuild over an existing, already-scored row", () => {
    const first = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [
        roomMessage(),
        fasteningRecord("f-1", replace("stanza-1", scores(0.6)), "2026-09-25T10:00:00Z"),
      ],
    });
    const reloaded = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [roomMessage()],
      existing: first,
    });
    expect(reloaded[0]?.safetyScores).toEqual(scores(0.6));
  });
});

describe("DM live merge", () => {
  test("applies to the open conversation's target row", () => {
    const messages = ref<TimelineMessage[]>([row({ id: "dm-1", stanzaId: "dm-stanza-1" })]);
    const liveMerge = useDmLiveMerge({
      session: ref(session),
      messages,
      activePeerJid: ref("bob@example.org"),
      pendingEchoClientIds: new Set<string>(),
      scrollToPinnedEdgeAndPin: mock(async () => true),
      persistLastSeen: mock(() => {}),
      isFeedVisible: () => true,
    });
    liveMerge.applySafetyScores(replace("dm-stanza-1", scores(0.3)));
    expect(messages.value[0]?.safetyScores).toEqual(scores(0.3));
  });
});

describe("BrowserXmppClient live dispatch", () => {
  type PrivateState = { handleMessage: (message: unknown) => void };

  function dmFasteningWasm(from: string) {
    return {
      mam_id: "f-dm-1",
      id: "f-dm-1",
      from,
      to: "alice@example.org/web",
      message_type: "chat",
      timestamp: "2026-09-25T10:00:00Z",
      reaction_emojis: [],
      shared_files: [],
      link_previews: [],
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      is_muc: false,
      safety_scores: {
        target_id: "dm-stanza-1",
        update: {
          kind: "replace",
          model_version: "typesafe/jev-1.13-20260917",
          scores: [{ category: "is_question", probability: 0.3, taxonomy_version: "is-question-v1" }],
        },
      },
    };
  }

  function client() {
    const instance = new BrowserXmppClient(session);
    const fastenings: SafetyScoresFastening[] = [];
    const directMessages: unknown[] = [];
    instance.setDmSafetyScoresHandler((fastening) => fastenings.push(fastening));
    instance.setDirectMessageHandler((message) => directMessages.push(message));
    return { state: instance as unknown as PrivateState, fastenings, directMessages };
  }

  test("a server-sent DM fastening emits the dedicated event, not a DM", () => {
    const h = client();
    h.state.handleMessage(dmFasteningWasm("example.org"));
    expect(h.fastenings.map((fastening) => fastening.targetId)).toEqual(["dm-stanza-1"]);
    expect(h.directMessages).toHaveLength(0);
  });

  test("a peer-sent DM fastening is dropped without surfacing anywhere", () => {
    const h = client();
    h.state.handleMessage(dmFasteningWasm("bob@example.org/phone"));
    expect(h.fastenings).toHaveLength(0);
    expect(h.directMessages).toHaveLength(0);
  });
});

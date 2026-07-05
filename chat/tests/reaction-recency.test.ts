// XEP-0444 Business Rules: a delayed reaction SHOULD be accepted only
// if no NEWER reaction from the same sender was already accepted (C5).
//
// The trigger in Waddle is the resume-barrier drain ordering: SM replays
// a delay-stamped old reaction into the resume buffer; MAM catch-up
// merges a newer reaction from the same sender first; the drain then
// applies the old one last and must NOT clobber the newer set.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelLiveMerge } from "../src/channels/live-merge";
import { buildChannelTimelineFromMamResults } from "../src/channels/message-timeline-state";
import { useDmLiveMerge } from "../src/dms/live-merge";
import { buildDmTimelineFromMamResults } from "../src/dms/message-timeline-state";
import type { LiveDmMessage, LiveRoomMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";

const session: WaddleSession = {
  username: "alice",
  jid: "alice@example.com/desktop",
  session_id: "tok",
  xmpp_websocket_url: "wss://example.com/ws",
};

const ROOM_JID = "room@muc.example.com";
const BOB = `${ROOM_JID}/bob`;

const OLDER = "2026-07-05T10:00:00.000Z";
const NEWER = "2026-07-05T10:05:00.000Z";

function channelHarness(seed: TimelineMessage[]) {
  const messages = ref<TimelineMessage[]>(seed);
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

function dmHarness(seed: TimelineMessage[]) {
  const messages = ref<TimelineMessage[]>(seed);
  const liveMerge = useDmLiveMerge({
    session: ref(session),
    messages,
    activePeerJid: ref("bob@example.com"),
    pendingEchoClientIds: new Set<string>(),
    scrollToPinnedEdgeAndPin: mock(async () => true),
    persistLastSeen: mock(() => {}),
    isFeedVisible: () => true,
  });
  return { messages, liveMerge };
}

function channelTarget(): TimelineMessage {
  return {
    id: "m1",
    reactionTargetId: "stanza-1",
    body: "hello",
    nick: "alice",
    timestamp: 0,
  } as TimelineMessage;
}

function dmTarget(): TimelineMessage {
  return { id: "m1", body: "hello", nick: "alice", timestamp: 0 } as TimelineMessage;
}

describe("XEP-0444 delayed-reaction recency (channel)", () => {
  test("an older delayed reaction from the same sender does not clobber a newer applied one", () => {
    const h = channelHarness([channelTarget()]);
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB, NEWER);
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB, OLDER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });

  test("older-then-newer still applies the newer set", () => {
    const h = channelHarness([channelTarget()]);
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB, OLDER);
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB, NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });

  test("a different sender's older reaction is unaffected", () => {
    const h = channelHarness([channelTarget()]);
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB, NEWER);
    h.liveMerge.applyReaction("stanza-1", "carol", ["👍"], `${ROOM_JID}/carol`, OLDER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"], "👍": ["carol"] });
  });

  test("a reaction without a timestamp still applies (live undelayed stanza)", () => {
    const h = channelHarness([channelTarget()]);
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB, NEWER);
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB);
    expect(h.messages.value[0]?.reactions).toEqual({ "👍": ["bob"] });
  });

  test("a MAM-merged newer reaction wins over a drained older live reaction", () => {
    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [
        {
          type: "message",
          roomJid: ROOM_JID,
          fromJid: BOB,
          nick: "bob",
          body: "",
          createdAt: NEWER,
          _reactionTarget: "stanza-1",
          _reactionEmojis: ["🎉"],
        } as unknown as LiveRoomMessage,
      ],
      existing: [channelTarget()],
    });
    const h = channelHarness(timeline);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });

    // Resume-barrier drain replays the delay-stamped OLD reaction last.
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB, OLDER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });
});

describe("XEP-0444 live undelayed reactions advance the recency stamp (channel)", () => {
  test("a re-delivered older delayed stanza cannot clobber a newer live undelayed reaction", () => {
    const h = channelHarness([channelTarget()]);
    // Delayed reaction T2 applies and stamps bob→T2.
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB, NEWER);
    // Bob's NEWER live undelayed reaction replaces the set.
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB);
    // The T2 stanza re-delivers (SM replay / overlapping MAM): it is
    // older than the live reaction and must not clobber it.
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB, NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "👍": ["bob"] });
  });

  test("exact same-stanza re-delivery is an idempotent no-op", () => {
    const h = channelHarness([channelTarget()]);
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB, NEWER);
    const before = h.messages.value;
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB, NEWER);
    expect(h.messages.value).toBe(before);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });
});

describe("XEP-0444 live undelayed reactions advance the recency stamp (DM)", () => {
  test("a re-delivered older delayed stanza cannot clobber a newer live undelayed reaction", () => {
    const h = dmHarness([dmTarget()]);
    h.liveMerge.applyReaction("m1", "bob", ["🎉"], NEWER);
    h.liveMerge.applyReaction("m1", "bob", ["👍"]);
    h.liveMerge.applyReaction("m1", "bob", ["🎉"], NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "👍": ["bob"] });
  });

  test("exact same-stanza re-delivery is an idempotent no-op", () => {
    const h = dmHarness([dmTarget()]);
    h.liveMerge.applyReaction("m1", "bob", ["🎉"], NEWER);
    const before = h.messages.value;
    h.liveMerge.applyReaction("m1", "bob", ["🎉"], NEWER);
    expect(h.messages.value).toBe(before);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });
});

describe("XEP-0444 delayed-reaction recency (DM)", () => {
  test("an older delayed reaction from the same sender does not clobber a newer applied one", () => {
    const h = dmHarness([dmTarget()]);
    h.liveMerge.applyReaction("m1", "bob", ["🎉"], NEWER);
    h.liveMerge.applyReaction("m1", "bob", ["👍"], OLDER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });

  test("older-then-newer still applies the newer set", () => {
    const h = dmHarness([dmTarget()]);
    h.liveMerge.applyReaction("m1", "bob", ["👍"], OLDER);
    h.liveMerge.applyReaction("m1", "bob", ["🎉"], NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });

  test("a MAM-merged newer reaction wins over a drained older live reaction", () => {
    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [
        {
          type: "message",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "",
          createdAt: NEWER,
          _reactionTarget: "m1",
          _reactionEmojis: ["🎉"],
        } as unknown as LiveDmMessage,
      ],
      existing: [dmTarget()],
    });
    const h = dmHarness(timeline);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });

    h.liveMerge.applyReaction("m1", "bob", ["👍"], OLDER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });
});

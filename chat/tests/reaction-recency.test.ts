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
const MIDDLE = "2026-07-05T10:02:00.000Z";
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

// F3: the server truncates XEP-0203 delay stamps to whole seconds, so a
// 👍→❤️ toggle within one second replays as two stanzas with IDENTICAL
// stamps. XEP-0444 rejects only if a strictly NEWER reaction was already
// accepted — on a tie, in-order delivery makes the later-processed
// update the newer one, so a different set must apply. Only an exact
// re-delivery (same set) stays an idempotent no-op.
describe("XEP-0444 equal-stamp tie applies a different set (F3)", () => {
  test("channel: a same-second toggle replayed with an identical stamp applies the final set", () => {
    const h = channelHarness([channelTarget()]);
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB, NEWER);
    h.liveMerge.applyReaction("stanza-1", "bob", ["❤️"], BOB, NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "❤️": ["bob"] });
  });

  test("channel: an exact re-delivery on the tie stays an idempotent no-op", () => {
    const h = channelHarness([channelTarget()]);
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍", "🎉"], BOB, NEWER);
    const before = h.messages.value;
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉", "👍"], BOB, NEWER);
    expect(h.messages.value).toBe(before);
    expect(h.messages.value[0]?.reactions).toEqual({ "👍": ["bob"], "🎉": ["bob"] });
  });

  test("dm: a same-second toggle replayed with an identical stamp applies the final set", () => {
    const h = dmHarness([dmTarget()]);
    h.liveMerge.applyReaction("m1", "bob", ["👍"], NEWER);
    h.liveMerge.applyReaction("m1", "bob", ["❤️"], NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "❤️": ["bob"] });
  });
});

// F4: live undelayed reactions must never stamp the LOCAL clock into a
// recency map that is compared against SERVER delay stamps — a client
// clock ahead by Δ would drop any genuinely newer reaction replayed
// within Δ. The wire stamps here are in the past relative to the test
// machine's clock, so the machine's clock IS "ahead" by construction.
describe("live undelayed stamps never mix clock domains (F4)", () => {
  test("channel: a replayed reaction newer than the last wire stamp applies despite a skewed-ahead local clock", () => {
    const h = channelHarness([channelTarget()]);
    // Wire-stamped 👍 at T0.
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB, OLDER);
    // Live undelayed toggle — recorded in the local domain.
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB);
    // The sender's genuinely newer reaction arrives via SM replay with
    // a server stamp far below the local clock. It must apply.
    h.liveMerge.applyReaction("stanza-1", "bob", ["❤️"], BOB, NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "❤️": ["bob"] });
  });

  test("channel: replays at or before the wire stamp the live reaction superseded stay stale", () => {
    const h = channelHarness([channelTarget()]);
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB, NEWER);
    h.liveMerge.applyReaction("stanza-1", "bob", ["🎉"], BOB);
    // Re-delivery of the superseded stanza and an even older one.
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], BOB, NEWER);
    h.liveMerge.applyReaction("stanza-1", "bob", ["😅"], BOB, OLDER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });

  test("dm: a replayed reaction newer than the last wire stamp applies despite a skewed-ahead local clock", () => {
    const h = dmHarness([dmTarget()]);
    h.liveMerge.applyReaction("m1", "bob", ["👍"], OLDER);
    h.liveMerge.applyReaction("m1", "bob", ["🎉"]);
    h.liveMerge.applyReaction("m1", "bob", ["❤️"], NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "❤️": ["bob"] });
  });
});

// F5: MUC recency must be keyed by the same senderId used for reaction
// replacement (real bare JID when known) — nicks are NOT stable. A nick
// rename must not let a re-delivered pre-rename stanza bypass the gate.
describe("MUC recency is keyed by senderId, not nick (F5)", () => {
  const REAL = "bob@example.com";

  test("a nick rename does not let a re-delivered pre-rename reaction clobber the newer set", () => {
    const h = channelHarness([channelTarget()]);
    // bob reacts pre-rename, then renames to bobby and reacts again
    // (same real JID).
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], REAL, OLDER);
    h.liveMerge.applyReaction("stanza-1", "bobby", ["🎉"], REAL, NEWER);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bobby"] });
    // MAM catch-up delivers a pre-rename toggle this client missed live,
    // under the OLD nick. Under nick-keyed recency it passes the gate
    // (nick bob's stamp is OLDER) and clobbers the newer post-rename
    // set; senderId-keyed recency (stamp NEWER) rejects it.
    h.liveMerge.applyReaction("stanza-1", "bob", ["😀"], REAL, MIDDLE);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bobby"] });
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

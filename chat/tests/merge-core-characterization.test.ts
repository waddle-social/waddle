// Characterization suite for the duplicated channel/DM merge cores
// (stage 1 of the merge-core unification). Every case runs against BOTH
// `useChannelLiveMerge` and `useDmLiveMerge` through thin adapters so the
// stage-2 extraction of shared logic has a behavioral safety net.
//
// Covered concerns: retraction (XEP-0424/0425), correction (XEP-0308),
// reactions (XEP-0444), displayed markers (XEP-0333), self-echo
// reconciliation, and ordering of out-of-order arrivals.
//
// Where the two pipelines intentionally diverge, the case carries
// separate `verifyChannel` / `verifyDm` expectations with a one-line
// comment. Divergences are documented as-is, not judged.

import { describe, expect, test } from "bun:test";
import { ref } from "vue";
import { useChannelLiveMerge } from "../src/channels/live-merge";
import { useDmLiveMerge } from "../src/dms/live-merge";
import type { MergeableMessage } from "../src/lib/messaging/types";
import type { LiveDmMessage, LiveRoomMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";

const ROOM = "room@muc.example.com";
const CHANNEL_ID = "general";
const PEER = "bob@example.com";
const SELF_NICK = "alice";

const T0 = "2026-05-14T10:00:00.000Z"; // established timeline rows
const T1 = "2026-05-14T10:05:00.000Z"; // default live arrival
const T_EARLY = "2026-05-14T09:55:00.000Z"; // out-of-order arrival

const session: WaddleSession = {
  username: SELF_NICK,
  jid: "alice@example.com/desktop",
  session_id: "tok",
  xmpp_websocket_url: "wss://example.com/ws",
};

type Pipeline = "channel" | "dm";

// The incoming-event spec is expressed in terms of the shared
// `MergeableMessage` shape; per-pipeline adapters widen it to the
// concrete wire type. The extra fields are pipeline-specific inputs.
type LiveSpec = Partial<MergeableMessage> & {
  /** Sender short name; "room" means the MUC bare JID (XEP-0425). */
  from?: string;
  /** Channel-only: MUC nick override (occupant-identity divergence cases). */
  channelNick?: string;
  /** Channel-only: XEP-0425 moderation target (dropped on the DM side). */
  moderationTargetId?: string;
  /** Channel-only: stanza type fed to the classifier. */
  stanzaType?: "message" | "subject";
  threadId?: string;
};

type MergeEvent =
  | { kind: "message"; spec: LiveSpec }
  | { kind: "reaction"; targetId: string; from: string; emojis: string[] }
  | { kind: "displayed"; messageId: string; reader: string };

interface PipelineOutcome {
  messages: TimelineMessage[];
  pendingEchoClientIds: Set<string>;
  persistCalls: [string, string][];
  scopeId: string;
}

interface MergeCase {
  name: string;
  pendingEchoIds?: string[];
  initial: (pipeline: Pipeline) => TimelineMessage[];
  events: MergeEvent[];
  /** Shared expectation when both pipelines agree. */
  verify?: (outcome: PipelineOutcome) => void;
  /** Divergent expectations — divergence documented, not judged. */
  verifyChannel?: (outcome: PipelineOutcome) => void;
  verifyDm?: (outcome: PipelineOutcome) => void;
}

function row(
  pipeline: Pipeline,
  author: string,
  overrides: Partial<TimelineMessage> & { id: string },
): TimelineMessage {
  return {
    author,
    authorJid: pipeline === "channel" ? `${ROOM}/${author}` : `${author}@example.com`,
    body: "original",
    createdAt: T0,
    createdAtSource: "archive",
    isSelf: author === SELF_NICK,
    ...overrides,
  };
}

function toRoomMessage(spec: LiveSpec): LiveRoomMessage {
  const nick = spec.channelNick ?? spec.from ?? "bob";
  const msg: LiveRoomMessage = {
    id: spec.id ?? "live-1",
    type: spec.stanzaType ?? "message",
    roomJid: ROOM,
    fromJid: spec.from === "room" ? ROOM : `${ROOM}/${nick}`,
    nick,
    body: spec.body ?? "",
    createdAt: spec.createdAt ?? T1,
    createdAtSource: spec.createdAtSource ?? "fallback",
  };
  if (spec.wireIds) msg.wireIds = spec.wireIds;
  if (spec.replacesId) msg.replacesId = spec.replacesId;
  if (spec.retractsId) msg.retractsId = spec.retractsId;
  if (spec.retractionId) msg.retractionId = spec.retractionId;
  if (spec.moderationTargetId) msg.moderationTargetId = spec.moderationTargetId;
  if (spec.linkPreviews) msg.linkPreviews = spec.linkPreviews;
  if (spec.threadId) msg.threadId = spec.threadId;
  return msg;
}

function toDmMessage(spec: LiveSpec): LiveDmMessage {
  const from = spec.from ?? "bob";
  const msg: LiveDmMessage = {
    id: spec.id ?? "live-1",
    // `LiveDmMessage["type"]` is the literal "message" — a subject stanza
    // is unrepresentable on the DM wire type. The cast exists solely to
    // characterize that the DM dispatch has no classifier gate at all.
    type: (spec.stanzaType ?? "message") as LiveDmMessage["type"],
    peerJid: PEER,
    fromJid: from === "room" ? ROOM : `${from}@example.com`,
    nick: from,
    body: spec.body ?? "",
    createdAt: spec.createdAt ?? T1,
    createdAtSource: spec.createdAtSource ?? "fallback",
  };
  if (spec.wireIds) msg.wireIds = spec.wireIds;
  if (spec.replacesId) msg.replacesId = spec.replacesId;
  if (spec.retractsId) msg.retractsId = spec.retractsId;
  if (spec.retractionId) msg.retractionId = spec.retractionId;
  if (spec.linkPreviews) msg.linkPreviews = spec.linkPreviews;
  if (spec.threadId) msg.threadId = spec.threadId;
  return msg;
}

function runPipeline(pipeline: Pipeline, testCase: MergeCase): PipelineOutcome {
  const messages = ref<TimelineMessage[]>(testCase.initial(pipeline));
  const pendingEchoClientIds = new Set(testCase.pendingEchoIds ?? []);
  const persistCalls: [string, string][] = [];
  const scrollToPinnedEdgeAndPin = async () => true;
  const persistLastSeen = (scopeId: string, messageId: string) => {
    persistCalls.push([scopeId, messageId]);
  };

  if (pipeline === "channel") {
    const merge = useChannelLiveMerge({
      session: ref(session),
      messages,
      activeChannelId: ref(CHANNEL_ID),
      pendingEchoClientIds,
      scrollToPinnedEdgeAndPin,
      persistLastSeen,
    });
    for (const event of testCase.events) {
      if (event.kind === "message") merge.handleRoomMessage(toRoomMessage(event.spec));
      else if (event.kind === "reaction") {
        merge.applyReaction(event.targetId, event.from, event.emojis, `${ROOM}/${event.from}`);
      } else merge.applyDisplayed(event.messageId, event.reader);
    }
    return { messages: messages.value, pendingEchoClientIds, persistCalls, scopeId: CHANNEL_ID };
  }

  const merge = useDmLiveMerge({
    session: ref(session),
    messages,
    activePeerJid: ref(PEER),
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
    isFeedVisible: (m) => !m.threadId || m.id === m.threadId,
  });
  for (const event of testCase.events) {
    if (event.kind === "message") merge.handleIncomingMessage(toDmMessage(event.spec));
    else if (event.kind === "reaction") merge.applyReaction(event.targetId, event.from, event.emojis);
    else merge.applyDisplayed(event.messageId, event.reader);
  }
  return { messages: messages.value, pendingEchoClientIds, persistCalls, scopeId: PEER };
}

const cases: MergeCase[] = [
  // ── XEP-0424 retraction ────────────────────────────────────────────
  {
    name: "retraction of a known target by its sender tombstones the message",
    initial: (p) => [row(p, "bob", {
      id: "m1",
      body: "hello",
      linkPreviews: [{ originalUrl: "https://old.example", title: "Old" }],
    })],
    events: [{ kind: "message", spec: { retractsId: "m1", retractionId: "r1", from: "bob" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.isRetracted).toBe(true);
      expect(o.messages[0]?.body).toBe("");
      expect(o.messages[0]?.retractionId).toBe("r1");
      expect(o.messages[0]?.linkPreviews).toBeUndefined();
    },
  },
  {
    name: "retraction of an unknown target is a no-op",
    initial: (p) => [row(p, "bob", { id: "m1", body: "hello" })],
    events: [{ kind: "message", spec: { retractsId: "nope", retractionId: "r1", from: "bob" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.isRetracted).toBeFalsy();
      expect(o.messages[0]?.body).toBe("hello");
    },
  },
  {
    name: "retraction from a different sender is refused (spoof attempt)",
    initial: (p) => [row(p, "bob", { id: "m1", body: "hello" })],
    events: [{ kind: "message", spec: { retractsId: "m1", retractionId: "r1", from: "mallory" } }],
    verify: (o) => {
      expect(o.messages[0]?.isRetracted).toBeFalsy();
      expect(o.messages[0]?.body).toBe("hello");
    },
  },
  {
    name: "retraction wins over correction when a stanza carries both",
    initial: (p) => [row(p, "bob", { id: "m1", body: "hello" })],
    events: [{
      kind: "message",
      spec: { retractsId: "m1", replacesId: "m1", body: "edited", retractionId: "r1", from: "bob" },
    }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.isRetracted).toBe(true);
      expect(o.messages[0]?.body).toBe("");
      expect(o.messages[0]?.isEdited).toBeFalsy();
    },
  },
  {
    // DIVERGENCE (documented, not judged): the channel gate requires the
    // retraction target id to match the row's XEP-0359 replyableId when
    // one exists; the DM gate retracts any id-resolvable row.
    name: "retraction whose target id differs from the row's replyableId",
    initial: (p) => [row(p, "bob", { id: "m1", body: "hello", replyableId: "stanza-other" })],
    events: [{ kind: "message", spec: { retractsId: "m1", retractionId: "r1", from: "bob" } }],
    verifyChannel: (o) => expect(o.messages[0]?.isRetracted).toBeFalsy(),
    verifyDm: (o) => expect(o.messages[0]?.isRetracted).toBe(true),
  },
  {
    // DIVERGENCE (documented, not judged): XEP-0425 service moderation
    // (retraction from the room bare JID) exists only on the channel
    // side; the DM side has no moderation concept and refuses on sender.
    name: "XEP-0425 moderation retraction from the service JID",
    initial: (p) => [row(p, "bob", {
      id: "m1",
      body: "hello",
      wireIds: ["stanza-1"],
      replyableId: "stanza-1",
    })],
    events: [{
      kind: "message",
      spec: { retractsId: "stanza-1", moderationTargetId: "stanza-1", retractionId: "mod-1", from: "room" },
    }],
    verifyChannel: (o) => {
      expect(o.messages[0]?.isRetracted).toBe(true);
      expect(o.messages[0]?.body).toBe("");
    },
    verifyDm: (o) => expect(o.messages[0]?.isRetracted).toBeFalsy(),
  },

  // ── XEP-0308 correction ────────────────────────────────────────────
  {
    name: "correction of a known target updates body (trimmed) and sets isEdited",
    initial: (p) => [row(p, "bob", { id: "m1", body: "original" })],
    events: [{ kind: "message", spec: { replacesId: "m1", body: "  edited  ", from: "bob" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.body).toBe("edited");
      expect(o.messages[0]?.isEdited).toBe(true);
    },
  },
  {
    name: "correction of an unknown target is a no-op and appends nothing",
    initial: (p) => [row(p, "bob", { id: "m1", body: "original" })],
    events: [{ kind: "message", spec: { replacesId: "nope", body: "edited", from: "bob" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.body).toBe("original");
      expect(o.messages[0]?.isEdited).toBeFalsy();
    },
  },
  {
    name: "correction from a different sender is refused (spoof attempt)",
    initial: (p) => [row(p, "bob", { id: "m1", body: "original" })],
    events: [{ kind: "message", spec: { replacesId: "m1", body: "hijacked!", from: "mallory" } }],
    verify: (o) => {
      expect(o.messages[0]?.body).toBe("original");
      expect(o.messages[0]?.isEdited).toBeFalsy();
    },
  },
  {
    name: "correction-of-correction targeting the original id applies on top",
    initial: (p) => [row(p, "bob", { id: "m1", body: "original" })],
    events: [
      { kind: "message", spec: { id: "corr-1", replacesId: "m1", body: "first edit", from: "bob" } },
      { kind: "message", spec: { id: "corr-2", replacesId: "m1", body: "second edit", from: "bob" } },
    ],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.body).toBe("second edit");
      expect(o.messages[0]?.isEdited).toBe(true);
    },
  },
  {
    name: "correction targeting a prior correction stanza's own id is a no-op",
    initial: (p) => [row(p, "bob", { id: "m1", body: "original" })],
    events: [
      { kind: "message", spec: { id: "corr-1", replacesId: "m1", body: "first edit", from: "bob" } },
      // Correction stanzas never materialize as timeline rows, so their
      // ids are unresolvable targets in both pipelines.
      { kind: "message", spec: { id: "corr-2", replacesId: "corr-1", body: "second edit", from: "bob" } },
    ],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.body).toBe("first edit");
    },
  },
  {
    name: "correction carrying link previews replaces the previous set wholesale",
    initial: (p) => [row(p, "bob", {
      id: "m1",
      body: "see https://old.example",
      linkPreviews: [{ originalUrl: "https://old.example", title: "Old" }],
    })],
    events: [{
      kind: "message",
      spec: {
        replacesId: "m1",
        body: "see https://new.example",
        from: "bob",
        linkPreviews: [{ originalUrl: "https://new.example", title: "New" }],
      },
    }],
    verify: (o) => {
      expect(o.messages[0]?.linkPreviews).toEqual([{ originalUrl: "https://new.example", title: "New" }]);
    },
  },
  {
    name: "correction without link previews clears stale preview state",
    initial: (p) => [row(p, "bob", {
      id: "m1",
      body: "see https://old.example",
      linkPreviews: [{ originalUrl: "https://old.example", title: "Old" }],
    })],
    events: [{ kind: "message", spec: { replacesId: "m1", body: "plain now", from: "bob" } }],
    verify: (o) => {
      expect(o.messages[0]?.body).toBe("plain now");
      expect(o.messages[0]?.linkPreviews).toBeUndefined();
    },
  },
  {
    // DIVERGENCE (documented, not judged): channel keys correction
    // identity on the MUC occupant JID (roomJid/nick), so a different
    // nick is a different author; DM keys on the bare JID, so any
    // resource/nick of the same account may correct.
    name: "correction from the same account under a different occupant nick",
    initial: (p) => [row(p, "bob", { id: "m1", body: "original" })],
    events: [{
      kind: "message",
      spec: { replacesId: "m1", body: "edited", from: "bob", channelNick: "bobby" },
    }],
    verifyChannel: (o) => {
      expect(o.messages[0]?.body).toBe("original");
      expect(o.messages[0]?.isEdited).toBeFalsy();
    },
    verifyDm: (o) => {
      expect(o.messages[0]?.body).toBe("edited");
      expect(o.messages[0]?.isEdited).toBe(true);
    },
  },

  // ── XEP-0444 reactions ─────────────────────────────────────────────
  {
    name: "reaction adds an emoji for the sender on a known target",
    initial: (p) => [row(p, "bob", { id: "m1", reactionTargetId: "m1" })],
    events: [{ kind: "reaction", targetId: "m1", from: "bob", emojis: ["👍"] }],
    verify: (o) => expect(o.messages[0]?.reactions).toEqual({ "👍": ["bob"] }),
  },
  {
    name: "reaction replace semantics: the sender's new set replaces the old one",
    initial: (p) => [row(p, "bob", {
      id: "m1",
      reactionTargetId: "m1",
      reactions: { "👍": ["bob"], "❤️": ["bob"] },
    })],
    events: [{ kind: "reaction", targetId: "m1", from: "bob", emojis: ["👍"] }],
    verify: (o) => expect(o.messages[0]?.reactions).toEqual({ "👍": ["bob"] }),
  },
  {
    name: "reaction with an empty set clears all of the sender's reactions",
    initial: (p) => [row(p, "bob", {
      id: "m1",
      reactionTargetId: "m1",
      reactions: { "👍": ["bob"] },
    })],
    events: [{ kind: "reaction", targetId: "m1", from: "bob", emojis: [] }],
    verify: (o) => expect(o.messages[0]?.reactions).toBeUndefined(),
  },
  {
    name: "reaction dedupe: re-sending the same emoji keeps a single entry per sender",
    initial: (p) => [row(p, "bob", {
      id: "m1",
      reactionTargetId: "m1",
      reactions: { "👍": ["bob"] },
    })],
    events: [{ kind: "reaction", targetId: "m1", from: "bob", emojis: ["👍"] }],
    verify: (o) => expect(o.messages[0]?.reactions).toEqual({ "👍": ["bob"] }),
  },
  {
    name: "reaction from a second sender accumulates on the same emoji",
    initial: (p) => [row(p, "bob", {
      id: "m1",
      reactionTargetId: "m1",
      reactions: { "👍": ["bob"] },
    })],
    events: [{ kind: "reaction", targetId: "m1", from: "carol", emojis: ["👍"] }],
    verify: (o) => expect(o.messages[0]?.reactions).toEqual({ "👍": ["bob", "carol"] }),
  },
  {
    name: "reaction targeting an unknown id is a no-op",
    initial: (p) => [row(p, "bob", { id: "m1", reactionTargetId: "m1" })],
    events: [{ kind: "reaction", targetId: "nope", from: "bob", emojis: ["👍"] }],
    verify: (o) => expect(o.messages[0]?.reactions).toBeUndefined(),
  },
  {
    // DIVERGENCE (documented, not judged): channel reactions resolve
    // strictly via the room-assigned stanza-id mirror (reactionTargetId);
    // DM reactions resolve via the row's primary id.
    name: "reaction targeting the primary id of a row without reactionTargetId",
    initial: (p) => [row(p, "bob", { id: "m1" })],
    events: [{ kind: "reaction", targetId: "m1", from: "bob", emojis: ["👍"] }],
    verifyChannel: (o) => expect(o.messages[0]?.reactions).toBeUndefined(),
    verifyDm: (o) => expect(o.messages[0]?.reactions).toEqual({ "👍": ["bob"] }),
  },
  {
    // DIVERGENCE (documented, not judged): DM reaction lookup also
    // resolves XEP-0359 wire aliases; channel lookup never consults
    // wireIds (only reactionTargetId).
    name: "reaction targeting a wire-id alias of the row",
    initial: (p) => [row(p, "bob", { id: "local-1", wireIds: ["m1"] })],
    events: [{ kind: "reaction", targetId: "m1", from: "bob", emojis: ["👍"] }],
    verifyChannel: (o) => expect(o.messages[0]?.reactions).toBeUndefined(),
    verifyDm: (o) => expect(o.messages[0]?.reactions).toEqual({ "👍": ["bob"] }),
  },
  {
    // DIVERGENCE (documented, not judged): channel keeps per-sender-id
    // reaction attribution (reactionSenders keyed by occupant JID); DM
    // keeps only the nick-keyed aggregate.
    name: "reaction sender bookkeeping (reactionSenders vs nick aggregate)",
    initial: (p) => [row(p, "bob", { id: "m1", reactionTargetId: "m1" })],
    events: [{ kind: "reaction", targetId: "m1", from: "bob", emojis: ["👍"] }],
    verifyChannel: (o) => {
      expect(o.messages[0]?.reactions).toEqual({ "👍": ["bob"] });
      expect(o.messages[0]?.reactionSenders).toEqual({ "👍": { [`${ROOM}/bob`]: "bob" } });
    },
    verifyDm: (o) => {
      expect(o.messages[0]?.reactions).toEqual({ "👍": ["bob"] });
      expect(o.messages[0]?.reactionSenders).toBeUndefined();
    },
  },

  // ── XEP-0333 displayed markers ─────────────────────────────────────
  {
    name: "displayed marker records the reader on the target row",
    initial: (p) => [row(p, SELF_NICK, { id: "m1" })],
    events: [{ kind: "displayed", messageId: "m1", reader: "bob" }],
    verify: (o) => expect(o.messages[0]?.readBy).toEqual(["bob"]),
  },
  {
    name: "displayed marker is idempotent per reader (never regresses or duplicates)",
    initial: (p) => [row(p, SELF_NICK, { id: "m1" })],
    events: [
      { kind: "displayed", messageId: "m1", reader: "bob" },
      { kind: "displayed", messageId: "m1", reader: "bob" },
    ],
    verify: (o) => expect(o.messages[0]?.readBy).toEqual(["bob"]),
  },
  {
    name: "displayed marker from a second reader advances readBy",
    initial: (p) => [row(p, SELF_NICK, { id: "m1", readBy: ["bob"] })],
    events: [{ kind: "displayed", messageId: "m1", reader: "carol" }],
    verify: (o) => expect(o.messages[0]?.readBy).toEqual(["bob", "carol"]),
  },
  {
    name: "displayed marker for an unknown id is a no-op",
    initial: (p) => [row(p, SELF_NICK, { id: "m1" })],
    events: [{ kind: "displayed", messageId: "nope", reader: "bob" }],
    verify: (o) => expect(o.messages[0]?.readBy).toBeUndefined(),
  },

  // ── self-echo reconciliation ───────────────────────────────────────
  {
    name: "self-echo reconciles the optimistic row by origin-id alias and promotes to delivered",
    initial: (p) => [row(p, SELF_NICK, {
      id: "client-1",
      wireIds: ["server-1"],
      body: "my msg",
      deliveryStatus: "sending",
      createdAtSource: "queued",
    })],
    events: [{ kind: "message", spec: { id: "server-1", from: SELF_NICK, body: "my msg" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.deliveryStatus).toBe("delivered");
      expect(o.messages[0]?.id).toBe("server-1");
      expect(o.messages[0]?.wireIds).toEqual(["client-1"]);
    },
  },
  {
    name: "self-echo body fallback reconciles a pending optimistic row and consumes the pending id",
    pendingEchoIds: ["client-1"],
    initial: (p) => [row(p, SELF_NICK, {
      id: "client-1",
      body: "dup body",
      deliveryStatus: "sending",
      createdAtSource: "queued",
    })],
    events: [{
      kind: "message",
      spec: { id: "d09c804f-f862-44df-8c7b-32e058cbf4ea", from: SELF_NICK, body: "dup body" },
    }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.deliveryStatus).toBe("delivered");
      expect(o.pendingEchoClientIds.has("client-1")).toBe(false);
    },
  },
  {
    name: "plain same-body echo never retargets an already delivered row (appends instead)",
    initial: (p) => [row(p, SELF_NICK, {
      id: "old-1",
      body: "dup body",
      deliveryStatus: "delivered",
    })],
    events: [{ kind: "message", spec: { id: "new-1", from: SELF_NICK, body: "dup body" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(2);
      expect(o.messages.map((m) => m.id).sort()).toEqual(["new-1", "old-1"]);
    },
  },
  {
    name: "echo carrying wire aliases skips the body-match fallback entirely",
    pendingEchoIds: ["client-1"],
    initial: (p) => [row(p, SELF_NICK, {
      id: "client-1",
      body: "dup body",
      deliveryStatus: "sending",
      createdAtSource: "queued",
    })],
    events: [{
      kind: "message",
      spec: { id: "server-9", wireIds: ["origin-9"], from: SELF_NICK, body: "dup body" },
    }],
    verify: (o) => {
      expect(o.messages).toHaveLength(2);
      expect(o.pendingEchoClientIds.has("client-1")).toBe(true);
    },
  },
  {
    name: "same-body message from a peer never reconciles a pending self row",
    pendingEchoIds: ["client-1"],
    initial: (p) => [row(p, SELF_NICK, {
      id: "client-1",
      body: "dup body",
      deliveryStatus: "sending",
      createdAtSource: "queued",
    })],
    events: [{ kind: "message", spec: { id: "peer-1", from: "bob", body: "dup body" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(2);
      expect(o.pendingEchoClientIds.has("client-1")).toBe(true);
    },
  },
  {
    name: "preserved-echo path: an untracked non-delivered self row (failed) reconciles by body",
    initial: (p) => [row(p, SELF_NICK, {
      id: "failed-1",
      body: "dup body",
      deliveryStatus: "failed",
      createdAtSource: "queued",
    })],
    events: [{ kind: "message", spec: { id: "server-1", from: SELF_NICK, body: "dup body" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.deliveryStatus).toBe("delivered");
      expect(o.messages[0]?.id).toBe("server-1");
    },
  },

  // ── ordering / duplicate arrivals ──────────────────────────────────
  {
    name: "out-of-order arrival: an older message lands before the newer existing row",
    initial: (p) => [row(p, "bob", { id: "newer", body: "newer", createdAt: T0 })],
    events: [{
      kind: "message",
      spec: { id: "older", from: "bob", body: "older", createdAt: T_EARLY, createdAtSource: "delay" },
    }],
    verify: (o) => expect(o.messages.map((m) => m.id)).toEqual(["older", "newer"]),
  },
  {
    name: "duplicate arrival by id merges in place and keeps the higher-authority timestamp",
    initial: (p) => [row(p, "bob", { id: "m1", body: "hello", createdAt: T0, createdAtSource: "archive" })],
    events: [{
      kind: "message",
      spec: { id: "m1", from: "bob", body: "hello", createdAt: T1, createdAtSource: "fallback" },
    }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.createdAt).toBe(T0);
      expect(o.messages[0]?.createdAtSource).toBe("archive");
    },
  },

  // ── dispatch / mapping divergences ─────────────────────────────────
  {
    // DIVERGENCE (documented, not judged): the channel pipeline runs the
    // classifyRoomMessage step and drops non-"message" stanza types; the
    // DM pipeline has no classifier (its wire type can only be "message"),
    // so an out-of-contract stanza would append.
    name: "non-message stanza type (channel-only classifyRoomMessage gate)",
    initial: () => [],
    events: [{ kind: "message", spec: { id: "s1", from: "bob", body: "topic!", stanzaType: "subject" } }],
    verifyChannel: (o) => expect(o.messages).toHaveLength(0),
    verifyDm: (o) => expect(o.messages).toHaveLength(1),
  },
  {
    // DIVERGENCE (documented, not judged): the channel merge applies
    // applyForumContext after every insert, inferring forumPostKind
    // "reply" for thread children; the DM merge has no forum concept.
    name: "threaded reply forum inference (channel-only applyForumContext)",
    initial: () => [],
    events: [{ kind: "message", spec: { id: "m2", from: "bob", body: "reply text", threadId: "t1" } }],
    verifyChannel: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.forumPostKind).toBe("reply");
    },
    verifyDm: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.messages[0]?.forumPostKind).toBeUndefined();
    },
  },

  // ── last-seen persistence on append ────────────────────────────────
  {
    name: "appending a feed-visible message persists last-seen for the active scope",
    initial: () => [],
    events: [{ kind: "message", spec: { id: "m9", from: "bob", body: "hi" } }],
    verify: (o) => expect(o.persistCalls).toEqual([[o.scopeId, "m9"]]),
  },
  {
    name: "appending a thread child does not persist last-seen",
    initial: () => [],
    events: [{ kind: "message", spec: { id: "m2", from: "bob", body: "reply", threadId: "t1" } }],
    verify: (o) => {
      expect(o.messages).toHaveLength(1);
      expect(o.persistCalls).toEqual([]);
    },
  },
];

describe("merge-core characterization (channel + dm pipelines)", () => {
  for (const testCase of cases) {
    const channelVerify = testCase.verifyChannel ?? testCase.verify;
    const dmVerify = testCase.verifyDm ?? testCase.verify;
    if (!channelVerify || !dmVerify) {
      throw new Error(`case "${testCase.name}" is missing an expectation`);
    }
    test(`channel: ${testCase.name}`, () => {
      channelVerify(runPipeline("channel", testCase));
    });
    test(`dm: ${testCase.name}`, () => {
      dmVerify(runPipeline("dm", testCase));
    });
  }
});

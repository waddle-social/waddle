import type { Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { LiveRoomMessage } from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import { applyForumContext, isFeedTimelineMessage, mapLiveRoomMessageToTimeline } from "@/channels/timeline";
import {
  channelReactedRow,
  isMucServiceModeration,
  isSameMucCorrectionSender,
  isSameMucRetractionSender,
  isValidMucModerationTarget,
  isValidMucRetractionTarget,
  mucCorrectionSender,
} from "@/channels/message-timeline-state";
import { applyCorrection as applyCorrectionUpdate } from "@/lib/messaging/correction";
import { applyDisplayedMarker } from "@/lib/messaging/displayed";
import { applyReactionUpdate, type ReactionPolicy } from "@/lib/messaging/reactions";
import { applyRetraction as applyRetractionUpdate, retractTimelineMessage } from "@/lib/messaging/retraction";
import { insertLiveMessage } from "@/lib/messaging/timeline-insert";
import { classifyRoomMessage } from "@/lib/xmpp/classify-room-message";
import { reportDisplayedMarkerFailure } from "@/lib/telemetry";

// Inbound merge composable for the channel side: applies incoming
// retractions (XEP-0424 / 0425), corrections (XEP-0308), reactions
// (XEP-0444), displayed markers (XEP-0333), and plain messages to the
// timeline. Self-echo reconciliation lives here too — it reads the
// `pendingEchoClientIds` Set from `useMucSend` so the body-match
// fallback only targets sends still awaiting their server-assigned id.
//
// The omnibus `setMessageHandler` from `BrowserXmppClient` is filtered
// and routed by `handleRoomMessage`, which delegates per-kind work to
// the per-XEP `apply*` helpers via the pure `classifyRoomMessage` pass.
// Once `BrowserXmppClient` exposes typed inbound events (follow-up from
// #351's plan), the classifier becomes redundant.

type UseChannelLiveMergeDeps = {
  session: Ref<WaddleSession | null>;
  messages: Ref<TimelineMessage[]>;
  activeChannelId: Ref<string | null>;
  pendingEchoClientIds: Set<string>;
  scrollToPinnedEdgeAndPin: () => Promise<boolean>;
  persistLastSeen: (channelId: string, messageId: string) => void;
};

const isFeedVisible = isFeedTimelineMessage;

// Divergences 4 + 5: strict `reactionTargetId` resolution and
// occupant-keyed sender bookkeeping (see `@/lib/messaging/reactions`).
const channelReactionPolicy: ReactionPolicy = {
  findTargetIndex: (msgs, targetId) => msgs.findIndex((m) => m.reactionTargetId === targetId),
  reactedRow: channelReactedRow,
};

export function useChannelLiveMerge(deps: UseChannelLiveMergeDeps) {
  const {
    session,
    messages,
    activeChannelId,
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
  } = deps;

  /** XEP-0333 displayed marker — shared merge, no channel divergences. */
  function applyDisplayed(messageId: string, nick: string) {
    const target = findMessageById(messages.value, messageId);
    if (!target) return;
    try {
      const next = applyDisplayedMarker(messages.value, messageId, nick);
      if (next) messages.value = next;
    } catch {
      reportDisplayedMarkerFailure({
        direction: "receive",
        kind: "room",
        reason: "receive-processing-failed",
        roundTripMs: elapsedSince(target.createdAt),
      });
    }
  }

  /**
   * XEP-0444 replace semantics via the shared merge; the channel policy
   * resolves targets strictly through the room-assigned stanza-id mirror
   * (`reactionTargetId`) and keeps occupant-keyed sender bookkeeping.
   */
  function applyReaction(messageId: string, nick: string, emojis: string[], senderId: string = nick, occurredAt?: string) {
    const next = applyReactionUpdate(
      messages.value,
      { targetId: messageId, nick, emojis, senderId, ...(occurredAt ? { occurredAt } : {}) },
      channelReactionPolicy,
    );
    if (next) messages.value = next;
  }

  /**
   * XEP-0424 retraction with XEP-0425 moderation support. The MUC-specific
   * gates (`isSameMucRetractionSender` / `isMucServiceModeration` /
   * replyableId matching) form the policy; the merge mechanics are shared.
   */
  function applyRetraction(msg: LiveRoomMessage) {
    const retractsId = msg.retractsId;
    if (!retractsId) return;
    const next = applyRetractionUpdate(
      messages.value,
      { retractsId, retractionId: msg.retractionId },
      {
        canRetract: (target) => {
          if (msg.moderationTargetId) {
            return isMucServiceModeration(msg)
              && isValidMucModerationTarget(target, msg.moderationTargetId);
          }
          return isValidMucRetractionTarget(target, retractsId)
            && isSameMucRetractionSender(target, mucCorrectionSender(msg));
        },
      },
    );
    if (next) messages.value = next;
  }

  /**
   * XEP-0308 last-message correction. Sender match enforced via
   * `isSameMucCorrectionSender` (real-JID for non-anon MUC, occupant-id
   * elsewhere) as the channel policy; the merge mechanics are shared.
   */
  function applyCorrection(
    replacesId: string,
    newBody: string,
    correctionSender: { authorJid: string; authorRealJid?: string },
    markup?: LiveRoomMessage["markup"],
    references?: LiveRoomMessage["references"],
    extensionAnnotations?: LiveRoomMessage["extensionAnnotations"],
    extensionBodyFallback?: boolean,
    linkPreviews?: LiveRoomMessage["linkPreviews"],
  ) {
    const next = applyCorrectionUpdate(
      messages.value,
      replacesId,
      { body: newBody, markup, references, linkPreviews, extensionAnnotations, extensionBodyFallback },
      { senderMatches: (target) => isSameMucCorrectionSender(target, correctionSender) },
    );
    if (next) messages.value = next;
  }

  function applyCallThreadEnded(ended: NonNullable<LiveRoomMessage["callThreadEnded"]>) {
    const index = messages.value.findIndex((message) =>
      !!message.callThread
      && (
        message.replyableId === ended.anchorId
        || message.stanzaId === ended.anchorId
        || message.id === ended.anchorId
        || message.wireIds?.includes(ended.anchorId)
      )
    );
    if (index < 0) return;
    const current = messages.value[index]!;
    if (!current.callThread) return;
    const next = messages.value.slice();
    next[index] = {
      ...current,
      callThread: {
        ...current.callThread,
        ended: ended.ended,
        duration: ended.duration,
      },
    };
    messages.value = next;
  }

  /**
   * Merges a regular incoming message (no retract, no correction) via the
   * shared insert. Self-echo reconciliation reads the
   * `pendingEchoClientIds` Set from `useMucSend`; the channel policy adds
   * the post-insert forum-context pass (divergence 7).
   */
  function mergeLiveMessage(rawMessage: TimelineMessage) {
    const msg = rawMessage.isRetracted
      ? retractTimelineMessage(rawMessage, rawMessage.retractionId)
      : rawMessage;
    const channelId = activeChannelId.value;
    const result = insertLiveMessage(messages.value, msg, pendingEchoClientIds, {
      finalize: (timeline) => applyForumContext(timeline),
    });
    messages.value = result.messages;
    if (!result.appended) return;
    void scrollToPinnedEdgeAndPin();
    if (channelId && isFeedVisible(msg)) {
      persistLastSeen(channelId, msg.id);
    }
  }

  /**
   * Routes an inbound `LiveRoomMessage` through the typed classifier and
   * dispatches to the per-XEP `apply*` helpers. Returns the classifier's
   * tagged output so callers (the orchestrator's notification logic) can
   * inspect the kind without re-classifying.
   */
  function handleRoomMessage(msg: LiveRoomMessage) {
    if (msg.callThreadEnded) {
      applyCallThreadEnded(msg.callThreadEnded);
      return { kind: "ignore" as const };
    }
    const classified = classifyRoomMessage(msg);
    switch (classified.kind) {
      case "retraction":
        applyRetraction(msg);
        break;
      case "correction":
        applyCorrection(
          classified.replacesId,
          classified.body,
          classified.sender,
          classified.markup,
          classified.references,
          classified.extensionAnnotations,
          classified.extensionBodyFallback,
          classified.linkPreviews,
        );
        break;
      case "live":
        if (session.value) {
          mergeLiveMessage(
            mapLiveRoomMessageToTimeline(session.value, msg, (id) => findMessageById(messages.value, id)),
          );
        }
        break;
      case "ignore":
        break;
    }
    return classified;
  }

  return {
    applyDisplayed,
    applyReaction,
    applyRetraction,
    applyCorrection,
    applyCallThreadEnded,
    mergeLiveMessage,
    handleRoomMessage,
  };
}

function elapsedSince(createdAt: string): number | null {
  const startedAt = Date.parse(createdAt);
  return Number.isFinite(startedAt) ? Math.max(0, Date.now() - startedAt) : null;
}

import type { Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { LiveDmMessage } from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById, findMessageIndexById } from "@/lib/message-ids";
import {
  dmReactedRow,
  fromLiveDmMessage,
  isSameDmCorrectionSender,
} from "@/dms/message-timeline-state";
import { applyCorrection as applyCorrectionUpdate } from "@/lib/messaging/correction";
import { applyDisplayedMarker } from "@/lib/messaging/displayed";
import { applyReactionUpdate, type ReactionPolicy } from "@/lib/messaging/reactions";
import { applyRetraction as applyRetractionUpdate, retractTimelineMessage } from "@/lib/messaging/retraction";
import { insertLiveMessage } from "@/lib/messaging/timeline-insert";
import { reportDisplayedMarkerFailure } from "@/lib/telemetry";

// Inbound merge composable for DMs: applies incoming retractions
// (XEP-0424), corrections (XEP-0308), reactions (XEP-0444), and displayed
// markers (XEP-0333) to the 1:1 timeline. Self-echo reconciliation uses
// `pendingEchoClientIds` from `useChatSend` so the body-match fallback
// only targets sends still awaiting their wire-stable id.
//
// DM-side already has typed event handlers from `BrowserXmppClient`
// (`onIncomingMessage`, `onChatState`, `onDisplayed`, `onReaction`), so no
// classifier is required — `handleIncomingMessage` just branches on
// `msg.retractsId` / `msg.replacesId` directly per the existing wire
// dispatch.

type UseDmLiveMergeDeps = {
  session: Ref<WaddleSession | null>;
  messages: Ref<TimelineMessage[]>;
  activePeerJid: Ref<string | null>;
  pendingEchoClientIds: Set<string>;
  scrollToPinnedEdgeAndPin: () => Promise<boolean>;
  persistLastSeen: (peerJid: string, messageId: string) => void;
  isFeedVisible: (m: TimelineMessage) => boolean;
};

// Divergences 4 + 5: primary-id + wire-alias resolution and the
// nick-keyed aggregate (see `@/lib/messaging/reactions`).
const dmReactionPolicy: ReactionPolicy = {
  findTargetIndex: findMessageIndexById,
  reactedRow: dmReactedRow,
};

export function useDmLiveMerge(deps: UseDmLiveMergeDeps) {
  const {
    session,
    messages,
    activePeerJid,
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
    isFeedVisible,
  } = deps;

  /** XEP-0333 displayed marker — shared merge, no DM divergences. */
  function applyDisplayed(messageId: string, nick: string) {
    const target = findMessageById(messages.value, messageId);
    if (!target) return;
    try {
      const next = applyDisplayedMarker(messages.value, messageId, nick);
      if (next) messages.value = next;
    } catch {
      reportDisplayedMarkerFailure({
        direction: "receive",
        kind: "dm",
        reason: "receive-processing-failed",
        roundTripMs: elapsedSince(target.createdAt),
      });
    }
  }

  /**
   * XEP-0444 replace semantics via the shared merge; the DM policy
   * resolves targets through the row's primary id plus XEP-0359 wire
   * aliases and keeps only the nick-keyed aggregate. (Carbon-forwarded
   * self reactions are attributed via the orchestrator-side
   * `peerNameFromJid` mapping in onReaction.)
   */
  function applyReaction(messageId: string, nick: string, emojis: string[], occurredAt?: string) {
    const next = applyReactionUpdate(
      messages.value,
      { targetId: messageId, nick, emojis, senderId: nick, ...(occurredAt ? { occurredAt } : {}) },
      dmReactionPolicy,
    );
    if (next) messages.value = next;
  }

  /**
   * XEP-0424 retraction. The bare-JID sender match is the DM policy; the
   * merge mechanics are shared. The DM side retracts any id-resolvable
   * row (no replyableId gate, no XEP-0425 moderation).
   */
  function applyRetraction(msg: LiveDmMessage) {
    if (!msg.retractsId) return;
    const next = applyRetractionUpdate(
      messages.value,
      { retractsId: msg.retractsId, retractionId: msg.retractionId },
      { canRetract: (target) => isSameDmCorrectionSender(target, msg.fromJid) },
    );
    if (next) messages.value = next;
  }

  /**
   * XEP-0308 last-message correction. Same bare-JID sender match as
   * retraction (the DM policy); the merge mechanics are shared.
   */
  function applyCorrection(
    replacesId: string,
    newBody: string,
    correctionFromJid: string,
    markup?: LiveDmMessage["markup"],
    references?: LiveDmMessage["references"],
    extensionAnnotations?: LiveDmMessage["extensionAnnotations"],
    extensionBodyFallback?: boolean,
    linkPreviews?: LiveDmMessage["linkPreviews"],
  ) {
    const next = applyCorrectionUpdate(
      messages.value,
      replacesId,
      { body: newBody, markup, references, linkPreviews, extensionAnnotations, extensionBodyFallback },
      { senderMatches: (target) => isSameDmCorrectionSender(target, correctionFromJid) },
    );
    if (next) messages.value = next;
  }

  /**
   * Plain inbound message via the shared insert — reconciles self-echo if
   * pending, else appends. No finalize pass (the DM side has no forum
   * concept — divergence 7 is channel-only).
   */
  function mergeLiveMessage(rawMessage: TimelineMessage) {
    const msg = rawMessage.isRetracted
      ? retractTimelineMessage(rawMessage, rawMessage.retractionId)
      : rawMessage;
    const peerJid = activePeerJid.value;
    const result = insertLiveMessage(messages.value, msg, pendingEchoClientIds);
    messages.value = result.messages;
    if (!result.appended) return;
    void scrollToPinnedEdgeAndPin();
    if (peerJid && isFeedVisible(msg)) {
      persistLastSeen(peerJid, msg.id);
    }
  }

  /**
   * Routes an inbound `LiveDmMessage` to the right `apply*` helper based
   * on the stanza's retract/correct/plain shape. Pre-filtered by the
   * orchestrator (active-peer check, typing-state clear).
   */
  function handleIncomingMessage(msg: LiveDmMessage) {
    if (msg.retractsId) {
      applyRetraction(msg);
      return;
    }
    if (msg.replacesId) {
      applyCorrection(
        msg.replacesId,
        msg.body,
        msg.fromJid,
        msg.markup,
        msg.references,
        msg.extensionAnnotations,
        msg.extensionBodyFallback,
        msg.linkPreviews,
      );
      return;
    }
    if (!session.value) return;
    mergeLiveMessage(
      fromLiveDmMessage(session.value, msg, (id) => findMessageById(messages.value, id)),
    );
  }

  return {
    applyDisplayed,
    applyReaction,
    applyRetraction,
    applyCorrection,
    mergeLiveMessage,
    handleIncomingMessage,
  };
}

function elapsedSince(createdAt: string): number | null {
  const startedAt = Date.parse(createdAt);
  return Number.isFinite(startedAt) ? Math.max(0, Date.now() - startedAt) : null;
}

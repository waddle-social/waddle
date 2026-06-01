import type { Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { LiveRoomMessage } from "@/lib/xmpp-client";
import {
  type DeliveryStatus,
  type TimelineMessage,
} from "@/lib/chat-ui";
import { findMessageById, findMessageIndexById, mergeMessageIds } from "@/lib/message-ids";
import { applyForumContext, isFeedTimelineMessage, mapLiveRoomMessageToTimeline } from "@/channels/timeline";
import {
  isMucServiceModeration,
  isSameMucCorrectionSender,
  isSameMucRetractionSender,
  isValidMucModerationTarget,
  isValidMucRetractionTarget,
  mucCorrectionSender,
  reactionSendersForUpdate,
  reactionsFromSenders,
  removeSenderReactions,
  retractChannelTimelineMessage,
} from "@/channels/message-timeline-state";
import { classifyRoomMessage } from "@/lib/xmpp/classify-room-message";
import { compareTimelineMessages, pickAuthoritativeTimestamp } from "@/lib/timeline-timestamps";
import {
  canUseSelfEchoBodyFallback,
} from "@/lib/self-echo-fallback";

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

export function useChannelLiveMerge(deps: UseChannelLiveMergeDeps) {
  const {
    session,
    messages,
    activeChannelId,
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
  } = deps;

  /**
   * XEP-0333: record the displayed marker (`<displayed/>` chat marker) for
   * a remote read of one of our messages. Cumulative — all earlier messages
   * are implicitly displayed for that reader too, but the helper only
   * mutates the exact target since the timeline carries `readBy` per
   * message. Short-circuits on id-miss to avoid allocating on every
   * inbound XEP-0333 marker.
   */
  function applyDisplayed(messageId: string, nick: string) {
    const index = findMessageIndexById(messages.value, messageId);
    if (index < 0) return;
    const current = messages.value[index]!;
    const existing = current.readBy ? [...current.readBy] : [];
    if (existing.includes(nick)) return;
    existing.push(nick);
    const next = messages.value.slice();
    next[index] = { ...current, readBy: existing };
    messages.value = next;
  }

  /**
   * XEP-0444 replace semantics: this author's full reaction set on the
   * target message becomes `emojis`. The previous per-author set is wiped
   * via `removeSenderReactions` so unticking an emoji actually removes it.
   * Short-circuits on target miss.
   */
  function applyReaction(messageId: string, nick: string, emojis: string[], senderId: string = nick) {
    const index = messages.value.findIndex((m) => m.reactionTargetId === messageId);
    if (index < 0) return;
    const current = messages.value[index]!;
    const reactionSenders = reactionSendersForUpdate(current, nick, senderId);
    removeSenderReactions(reactionSenders, nick, senderId);
    for (const emoji of emojis) {
      if (!reactionSenders[emoji]) reactionSenders[emoji] = {};
      reactionSenders[emoji][senderId] = nick;
    }
    const reactions = reactionsFromSenders(reactionSenders);
    const updated: TimelineMessage = { ...current };
    if (Object.keys(reactionSenders).length > 0) {
      updated.reactionSenders = reactionSenders;
      updated.reactions = reactions;
    } else {
      delete updated.reactionSenders;
      delete updated.reactions;
    }
    const next = messages.value.slice();
    next[index] = updated;
    messages.value = next;
  }

  /**
   * XEP-0424 retraction with XEP-0425 moderation support. Sender gating
   * happens here via `isSameMucRetractionSender` / `isMucServiceModeration`
   * so a spoofed retraction from a different occupant can't tombstone
   * someone else's message. Short-circuits on target miss / sender mismatch.
   */
  function applyRetraction(msg: LiveRoomMessage) {
    if (!msg.retractsId) return;
    const index = findMessageIndexById(messages.value, msg.retractsId);
    if (index < 0) return;
    const target = messages.value[index]!;
    if (msg.moderationTargetId) {
      if (!isMucServiceModeration(msg)) return;
      if (!isValidMucModerationTarget(target, msg.moderationTargetId)) return;
    } else if (
      !isValidMucRetractionTarget(target, msg.retractsId)
      || !isSameMucRetractionSender(target, mucCorrectionSender(msg))
    ) {
      return;
    }
    const next = messages.value.slice();
    next[index] = retractChannelTimelineMessage(target, msg.retractionId);
    messages.value = next;
  }

  /**
   * XEP-0308 last-message correction. Sender match enforced via
   * `isSameMucCorrectionSender` (real-JID for non-anon MUC, occupant-id
   * elsewhere). Markup / references / extension annotations are replaced
   * wholesale to mirror the wire shape — null/empty drops the field.
   * Short-circuits on target miss / sender mismatch.
   */
  function applyCorrection(
    replacesId: string,
    newBody: string,
    correctionSender: { authorJid: string; authorRealJid?: string },
    markup?: LiveRoomMessage["markup"],
    references?: LiveRoomMessage["references"],
    extensionAnnotations?: LiveRoomMessage["extensionAnnotations"],
    extensionBodyFallback?: boolean,
  ) {
    const candidateIndex = findMessageIndexById(messages.value, replacesId);
    if (candidateIndex < 0) return;
    if (!isSameMucCorrectionSender(messages.value[candidateIndex]!, correctionSender)) return;
    const index = candidateIndex;
    const current = messages.value[index]!;
    const updated: TimelineMessage = { ...current, body: newBody.trim(), isEdited: true };
    if (markup && markup.length > 0) updated.markup = markup;
    else delete updated.markup;
    if (references && references.length > 0) updated.references = references;
    else delete updated.references;
    if (extensionAnnotations && extensionAnnotations.length > 0) {
      updated.extensionAnnotations = extensionAnnotations;
      if (extensionBodyFallback) updated.extensionBodyFallback = true;
      else delete updated.extensionBodyFallback;
    } else {
      delete updated.extensionAnnotations;
      delete updated.extensionBodyFallback;
    }
    const next = messages.value.slice();
    next[index] = updated;
    messages.value = next;
  }

  /**
   * Merges a regular incoming message (no retract, no correction). If the
   * message reconciles a still-pending self-echo we promote the existing
   * optimistic entry to "delivered" via the `pendingEchoClientIds` body
   * match. Otherwise we append.
   */
  function mergeLiveMessage(rawMessage: TimelineMessage) {
    const msg = rawMessage.isRetracted
      ? retractChannelTimelineMessage(rawMessage, rawMessage.retractionId)
      : rawMessage;
    const existingById = [msg.id, ...(msg.wireIds ?? [])]
      .map((id) => findMessageById(messages.value, id))
      .find((message): message is TimelineMessage => !!message);
    const bodyFallbackAllowed = !existingById && canUseSelfEchoBodyFallback(msg);
    const pendingSelfEcho = bodyFallbackAllowed
      ? messages.value.find(
        (m) =>
          pendingEchoClientIds.has(m.id)
          && m.isSelf
          && m.body === msg.body,
      )
      : undefined;
    const preservedSelfEcho = bodyFallbackAllowed
      ? [...messages.value].reverse().find(
        (m) =>
          m.isSelf
          && m.body === msg.body
          && !!m.deliveryStatus
          && m.deliveryStatus !== "delivered",
      )
      : undefined;
    const existing = existingById ?? pendingSelfEcho ?? preservedSelfEcho;
    if (existing) {
      messages.value = messages.value.map((m) => {
        if (m.id !== existing.id) return m;
        const mergedIds = mergeMessageIds(m, msg.id, msg.wireIds);
        // Pick the more authoritative `createdAt` so a redelivered
        // live stanza (`fallback`) can't overwrite a true server
        // stamp (`archive`/`delay`) that landed via MAM first. See
        // `dms/live-merge.ts` for the same rule.
        const authoritativeTimestamp = pickAuthoritativeTimestamp(
          { createdAt: m.createdAt, createdAtSource: m.createdAtSource },
          { createdAt: msg.createdAt, createdAtSource: msg.createdAtSource },
        );
        const updated: TimelineMessage = {
          ...m,
          ...msg,
          id: mergedIds.id,
          createdAt: authoritativeTimestamp.createdAt,
          createdAtSource: authoritativeTimestamp.createdAtSource,
        };
        if (!Object.prototype.hasOwnProperty.call(msg, "linkPreviews")) {
          delete updated.linkPreviews;
        }
        if (mergedIds.wireIds?.length) updated.wireIds = mergedIds.wireIds;
        else delete updated.wireIds;
        if (m.isSelf && msg.isSelf) {
          // A self-echo from the room is authoritative; it supersedes any
          // prior "sending" / "failed" optimistic state.
          updated.deliveryStatus = "delivered" as DeliveryStatus;
        }
        return updated;
      }).sort(compareTimelineMessages);
      messages.value = applyForumContext(messages.value);
      // Drop every pending optimistic id this reconciliation accounted
      // for — both the previous primary id and any pre-existing wire
      // aliases — so a later same-body replay within the fallback window
      // can never retarget the now-delivered row.
      pendingEchoClientIds.delete(existing.id);
      for (const alias of existing.wireIds ?? []) pendingEchoClientIds.delete(alias);
      return;
    }
    const channelId = activeChannelId.value;
    messages.value = applyForumContext([...messages.value, msg].sort(compareTimelineMessages));
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
    mergeLiveMessage,
    handleRoomMessage,
  };
}

import type { Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { LiveRoomMessage } from "@/lib/xmpp-client";
import {
  type DeliveryStatus,
  type TimelineMessage,
} from "@/lib/chat-ui";
import { findMessageById, matchMessageId, mergeMessageIds } from "@/lib/message-ids";
import { applyForumContext, isFeedTimelineMessage, mapLiveRoomMessageToTimeline } from "@/channels/timeline";
import {
  isMucServiceModeration,
  isSameMucCorrectionSender,
  isSameMucRetractionSender,
  isValidMucModerationTarget,
  isValidMucRetractionTarget,
  reactionSendersForUpdate,
  reactionsFromSenders,
  removeSenderReactions,
  retractChannelTimelineMessage,
} from "@/channels/message-timeline-state";
import { classifyRoomMessage } from "@/lib/xmpp/classify-room-message";

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
   * message.
   */
  function applyDisplayed(messageId: string, nick: string) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (!matchMessageId(m, messageId)) return m;
      const existing = m.readBy ? [...m.readBy] : [];
      if (!existing.includes(nick)) existing.push(nick);
      return { ...m, readBy: existing };
    });
  }

  /**
   * XEP-0444 replace semantics: this author's full reaction set on the
   * target message becomes `emojis`. The previous per-author set is wiped
   * via `removeSenderReactions` so unticking an emoji actually removes it.
   */
  function applyReaction(messageId: string, nick: string, emojis: string[], senderId: string = nick) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (m.reactionTargetId !== messageId) return m;
      const reactionSenders = reactionSendersForUpdate(m, nick, senderId);
      removeSenderReactions(reactionSenders, nick, senderId);
      for (const emoji of emojis) {
        if (!reactionSenders[emoji]) reactionSenders[emoji] = {};
        reactionSenders[emoji][senderId] = nick;
      }
      const reactions = reactionsFromSenders(reactionSenders);
      const updated = { ...m };
      if (Object.keys(reactionSenders).length > 0) {
        updated.reactionSenders = reactionSenders;
        updated.reactions = reactions;
      } else {
        delete updated.reactionSenders;
        delete updated.reactions;
      }
      return updated;
    });
  }

  /**
   * XEP-0424 retraction with XEP-0425 moderation support. Sender gating
   * happens here via `isSameMucRetractionSender` / `isMucServiceModeration`
   * so a spoofed retraction from a different occupant can't tombstone
   * someone else's message.
   */
  function applyRetraction(msg: LiveRoomMessage) {
    messages.value = messages.value.map((m) => {
      if (!msg.retractsId || !matchMessageId(m, msg.retractsId)) return m;
      if (msg.moderationTargetId) {
        if (!isMucServiceModeration(msg)) return m;
        if (!isValidMucModerationTarget(m, msg.moderationTargetId)) return m;
      } else if (
        !isValidMucRetractionTarget(m, msg.retractsId)
        || !isSameMucRetractionSender(m, mucCorrectionSenderFor(msg))
      ) {
        return m;
      }
      return retractChannelTimelineMessage(m, msg.retractionId);
    });
  }

  /**
   * XEP-0308 last-message correction. Sender match enforced via
   * `isSameMucCorrectionSender` (real-JID for non-anon MUC, occupant-id
   * elsewhere). Markup / references / extension annotations are replaced
   * wholesale to mirror the wire shape — null/empty drops the field.
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
    const idx = messages.value.findIndex((m) =>
      matchMessageId(m, replacesId) && isSameMucCorrectionSender(m, correctionSender)
    );
    if (idx === -1) return;
    messages.value = messages.value.map((m) => {
      if (!matchMessageId(m, replacesId) || !isSameMucCorrectionSender(m, correctionSender)) return m;
      const updated: TimelineMessage = { ...m, body: newBody.trim(), isEdited: true };
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
      return updated;
    });
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
    const pendingSelfEcho = messages.value.find(
      (m) => pendingEchoClientIds.has(m.id) && m.isSelf && msg.isSelf && m.body === msg.body,
    );
    const preservedSelfEcho = [...messages.value].reverse().find(
      (m) =>
        m.isSelf
        && msg.isSelf
        && m.body === msg.body
        && !!m.deliveryStatus
        && m.deliveryStatus !== "delivered",
    );
    const existing = existingById ?? pendingSelfEcho ?? preservedSelfEcho;
    if (existing) {
      const wasPending = existing.id !== msg.id;
      messages.value = messages.value.map((m) => {
        if (m.id !== existing.id) return m;
        const updated: TimelineMessage = {
          ...m,
          ...msg,
          ...mergeMessageIds(m, msg.id, msg.wireIds),
        };
        if (m.isSelf && msg.isSelf) {
          // A self-echo from the room is authoritative; it supersedes any
          // prior "sending" / "failed" optimistic state.
          updated.deliveryStatus = "delivered" as DeliveryStatus;
        }
        return updated;
      });
      messages.value = applyForumContext(messages.value);
      if (wasPending) pendingEchoClientIds.delete(existing.id);
      return;
    }
    const channelId = activeChannelId.value;
    messages.value = applyForumContext([...messages.value, msg]);
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

// Local helper to avoid a cross-file circular: mucCorrectionSender lives in
// message-timeline-state and is also used by the classifier. We keep a
// thin local function here so applyRetraction doesn't reach into the
// classifier's typed output for the sender — it's still building it from
// the raw stanza.
function mucCorrectionSenderFor(msg: LiveRoomMessage): { authorJid: string; authorRealJid?: string } {
  return {
    authorJid: `${msg.roomJid}/${msg.nick}`,
    ...(msg.authorRealJid ? { authorRealJid: msg.authorRealJid } : {}),
  };
}

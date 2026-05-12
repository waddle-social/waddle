import type { Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { LiveDmMessage } from "@/lib/xmpp-client";
import {
  type DeliveryStatus,
  type TimelineMessage,
} from "@/lib/chat-ui";
import { findMessageById, matchMessageId, mergeMessageIds } from "@/lib/message-ids";
import {
  fromLiveDmMessage,
  isSameDmCorrectionSender,
  retractDmTimelineMessage,
} from "@/dms/message-timeline-state";

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

  /** XEP-0333 displayed marker: append the reader's nick to `readBy`. */
  function applyDisplayed(messageId: string, nick: string) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (!matchMessageId(m, messageId)) return m;
      const existing = m.readBy ? [...m.readBy] : [];
      if (!existing.includes(nick)) existing.push(nick);
      return { ...m, readBy: existing };
    });
  }

  /**
   * XEP-0444 replace semantics for DMs. No real-JID sender attribution —
   * the DM stanza already arrives keyed on the conversation partner, so
   * the reaction-set per nick is sufficient. (Carbon-forwarded self
   * reactions are attributed via the orchestrator-side `peerNameFromJid`
   * mapping in onReaction.)
   */
  function applyReaction(messageId: string, nick: string, emojis: string[]) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (!matchMessageId(m, messageId)) return m;
      const existing: Record<string, string[]> = m.reactions ? { ...m.reactions } : {};
      for (const key of Object.keys(existing)) {
        existing[key] = (existing[key] ?? []).filter((n) => n !== nick);
        if (existing[key].length === 0) delete existing[key];
      }
      for (const emoji of emojis) {
        if (!existing[emoji]) existing[emoji] = [];
        existing[emoji].push(nick);
      }
      const updated = { ...m };
      if (Object.keys(existing).length > 0) updated.reactions = existing;
      else delete updated.reactions;
      return updated;
    });
  }

  /** XEP-0424 retraction. Sender match via `isSameDmCorrectionSender`. */
  function applyRetraction(msg: LiveDmMessage) {
    messages.value = messages.value.map((m) =>
      msg.retractsId && matchMessageId(m, msg.retractsId) && isSameDmCorrectionSender(m, msg.fromJid)
        ? retractDmTimelineMessage(m, msg.retractionId)
        : m
    );
  }

  /** XEP-0308 last-message correction. Same sender match as retraction. */
  function applyCorrection(
    replacesId: string,
    newBody: string,
    correctionFromJid: string,
    markup?: LiveDmMessage["markup"],
    references?: LiveDmMessage["references"],
    extensionAnnotations?: LiveDmMessage["extensionAnnotations"],
    extensionBodyFallback?: boolean,
  ) {
    messages.value = messages.value.map((m) => {
      if (!matchMessageId(m, replacesId) || !isSameDmCorrectionSender(m, correctionFromJid)) return m;
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

  /** Plain inbound message — reconciles self-echo if pending, else appends. */
  function mergeLiveMessage(rawMessage: TimelineMessage) {
    const msg = rawMessage.isRetracted
      ? retractDmTimelineMessage(rawMessage, rawMessage.retractionId)
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
          updated.deliveryStatus = "delivered" as DeliveryStatus;
        }
        return updated;
      });
      if (wasPending) pendingEchoClientIds.delete(existing.id);
      return;
    }
    const peerJid = activePeerJid.value;
    messages.value = [...messages.value, msg];
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

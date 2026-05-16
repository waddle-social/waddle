import type { Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { LiveDmMessage } from "@/lib/xmpp-client";
import {
  type DeliveryStatus,
  type TimelineMessage,
} from "@/lib/chat-ui";
import { findMessageById, findMessageIndexById, mergeMessageIds } from "@/lib/message-ids";
import {
  fromLiveDmMessage,
  isSameDmCorrectionSender,
  retractDmTimelineMessage,
} from "@/dms/message-timeline-state";
import { compareTimelineMessages } from "@/lib/timeline-timestamps";
import {
  canUseSelfEchoBodyFallback,
  withinSelfEchoFallbackWindow,
} from "@/lib/self-echo-fallback";

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

  /**
   * XEP-0333 displayed marker: append the reader's nick to `readBy`.
   * Short-circuits on target miss / already-marked.
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
   * XEP-0444 replace semantics for DMs. No real-JID sender attribution —
   * the DM stanza already arrives keyed on the conversation partner, so
   * the reaction-set per nick is sufficient. (Carbon-forwarded self
   * reactions are attributed via the orchestrator-side `peerNameFromJid`
   * mapping in onReaction.) Short-circuits on target miss.
   */
  function applyReaction(messageId: string, nick: string, emojis: string[]) {
    const index = findMessageIndexById(messages.value, messageId);
    if (index < 0) return;
    const current = messages.value[index]!;
    const existing: Record<string, string[]> = current.reactions ? { ...current.reactions } : {};
    for (const key of Object.keys(existing)) {
      existing[key] = (existing[key] ?? []).filter((n) => n !== nick);
      if (existing[key].length === 0) delete existing[key];
    }
    for (const emoji of emojis) {
      if (!existing[emoji]) existing[emoji] = [];
      existing[emoji].push(nick);
    }
    const updated: TimelineMessage = { ...current };
    if (Object.keys(existing).length > 0) updated.reactions = existing;
    else delete updated.reactions;
    const next = messages.value.slice();
    next[index] = updated;
    messages.value = next;
  }

  /**
   * XEP-0424 retraction. Sender match via `isSameDmCorrectionSender`
   * — short-circuits on target miss / sender mismatch.
   */
  function applyRetraction(msg: LiveDmMessage) {
    if (!msg.retractsId) return;
    const candidateIndex = findMessageIndexById(messages.value, msg.retractsId);
    if (candidateIndex < 0) return;
    if (!isSameDmCorrectionSender(messages.value[candidateIndex]!, msg.fromJid)) return;
    const next = messages.value.slice();
    next[candidateIndex] = retractDmTimelineMessage(messages.value[candidateIndex]!, msg.retractionId);
    messages.value = next;
  }

  /**
   * XEP-0308 last-message correction. Same sender match as retraction.
   * Short-circuits on target miss / sender mismatch.
   */
  function applyCorrection(
    replacesId: string,
    newBody: string,
    correctionFromJid: string,
    markup?: LiveDmMessage["markup"],
    references?: LiveDmMessage["references"],
    extensionAnnotations?: LiveDmMessage["extensionAnnotations"],
    extensionBodyFallback?: boolean,
  ) {
    const candidateIndex = findMessageIndexById(messages.value, replacesId);
    if (candidateIndex < 0) return;
    if (!isSameDmCorrectionSender(messages.value[candidateIndex]!, correctionFromJid)) return;
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

  /** Plain inbound message — reconciles self-echo if pending, else appends. */
  function mergeLiveMessage(rawMessage: TimelineMessage) {
    const msg = rawMessage.isRetracted
      ? retractDmTimelineMessage(rawMessage, rawMessage.retractionId)
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
          && m.body === msg.body
          && withinSelfEchoFallbackWindow(m, msg),
      )
      : undefined;
    const preservedSelfEcho = bodyFallbackAllowed
      ? [...messages.value].reverse().find(
        (m) =>
          m.isSelf
          && m.body === msg.body
          && !!m.deliveryStatus
          && m.deliveryStatus !== "delivered"
          && withinSelfEchoFallbackWindow(m, msg),
      )
      : undefined;
    const existing = existingById ?? pendingSelfEcho ?? preservedSelfEcho;
    if (existing) {
      messages.value = messages.value.map((m) => {
        if (m.id !== existing.id) return m;
        const mergedIds = mergeMessageIds(m, msg.id, msg.wireIds);
        const updated: TimelineMessage = {
          ...m,
          ...msg,
          id: mergedIds.id,
        };
        if (mergedIds.wireIds?.length) updated.wireIds = mergedIds.wireIds;
        else delete updated.wireIds;
        if (m.isSelf && msg.isSelf) {
          updated.deliveryStatus = "delivered" as DeliveryStatus;
        }
        return updated;
      }).sort(compareTimelineMessages);
      // Drop every pending optimistic id this reconciliation accounted
      // for — both the previous primary id and any pre-existing wire
      // aliases — so a later same-body replay within the fallback window
      // can never retarget the now-delivered row.
      pendingEchoClientIds.delete(existing.id);
      for (const alias of existing.wireIds ?? []) pendingEchoClientIds.delete(alias);
      return;
    }
    const peerJid = activePeerJid.value;
    messages.value = [...messages.value, msg].sort(compareTimelineMessages);
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

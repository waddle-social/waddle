import type { TimelineMessage } from "@/lib/chat-ui";
import {
  applyDeliveryEvent,
  type DeliveryEvent,
} from "@/lib/xmpp/delivery-lifecycle";

// Shared UI-mutation helpers used by both the channel-side useMucSend and
// the DM-side useChatSend composables. Operates on TimelineMessage arrays
// without depending on Vue, MUC vs 1:1 semantics, or the XMPP client — so
// each helper is testable in isolation and reusable across both surfaces.
//
// Helpers that are MUC-specific (e.g. sender comparison via real-JID + nick)
// belong in channels/message-timeline-state.ts; helpers that are DM-specific
// in dms/message-timeline-state.ts; this file is the home for the
// genuinely-cross-side operations identified during the PR 1/PR 2 design
// grilling for #351.

/**
 * Apply a delivery event to the message matching `messageId`, but only when
 * the message is a self-send. Non-self messages are untouched because the
 * delivery lifecycle tracks the local outbound stanza, not received ones.
 *
 * The helper preserves reference identity when the result would be
 * structurally identical to the input — both at the array level (id miss
 * or terminal-no-op) and at the element level (no-downgrade short-circuits
 * to the existing object). Vue's reactivity skips re-renders in that case.
 */
export function applyDeliveryEventById<M extends TimelineMessage>(
  timeline: ReadonlyArray<M>,
  messageId: string,
  event: DeliveryEvent,
): M[] {
  let changed = false;
  const next = timeline.map((m): M => {
    if (m.id !== messageId || !m.isSelf) return m;
    const nextStatus = applyDeliveryEvent(m.deliveryStatus, event);
    if (nextStatus === m.deliveryStatus) return m;
    changed = true;
    return { ...m, deliveryStatus: nextStatus };
  });
  return changed ? next : (timeline as M[]);
}

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
 * Apply a delivery event to the self-message matching `messageId`. Returns
 * the original array reference when nothing changes (id miss or
 * no-downgrade short-circuit), so Vue's reactivity skips a re-render and
 * downstream watchers don't fire.
 *
 * Optimised for the XEP-0184 / XEP-0198 fan-out hot path: ack, queue, and
 * failure events arrive on every outbound stanza id and fan out to both
 * the room and DM timelines. The previous implementation always allocated
 * via `Array.map`; this version locates the target via `findIndex` and
 * only allocates when the transition is real.
 *
 * Generic `M extends TimelineMessage` accepts mutable `M[]` and returns
 * `M[]` — callers operate on a Vue `Ref<TimelineMessage[]>` whose `.value`
 * is mutable, so the input is `M[]` rather than `ReadonlyArray<M>`.
 */
export function applyDeliveryEventById<M extends TimelineMessage>(
  timeline: M[],
  messageId: string,
  event: DeliveryEvent,
): M[] {
  const index = timeline.findIndex((m) => m.id === messageId && m.isSelf);
  if (index < 0) return timeline;
  const current = timeline[index]!;
  const nextStatus = applyDeliveryEvent(current.deliveryStatus, event);
  if (nextStatus === current.deliveryStatus) return timeline;
  const next = timeline.slice();
  next[index] = { ...current, deliveryStatus: nextStatus };
  return next;
}

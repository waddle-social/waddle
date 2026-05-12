import type { TimelineMessage } from "@/lib/chat-ui";
import { matchMessageId } from "@/lib/message-ids";
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
 * Find the id of the most recent remote (non-self, non-retracted) message
 * in the timeline. Used by XEP-0333 displayed-marker logic to derive the
 * "latest message we should mark as read" anchor — a self-sent or
 * tombstoned message doesn't carry that semantic so we walk back past
 * them. Returns null when the timeline is empty or contains only
 * self/retracted entries.
 */
export function latestRemoteMessageIdFor(timeline: ReadonlyArray<TimelineMessage>): string | null {
  for (let i = timeline.length - 1; i >= 0; i--) {
    const m = timeline[i];
    if (!m || m.isSelf || m.isRetracted) continue;
    return m.id;
  }
  return null;
}

/**
 * Apply a delivery event to the self-message matching `messageId`. Returns
 * the original array reference when nothing changes (id miss or
 * no-downgrade short-circuit), so Vue's reactivity skips a re-render and
 * downstream watchers don't fire.
 *
 * Matches the id via `matchMessageId` (primary id OR a XEP-0359-reconciled
 * id in `wireIds`). XEP-0198 SM acks and the wasm queue/failure callbacks
 * fire with the client-assigned id, but by the time they arrive the
 * message's primary id may already have been swapped to the room/server
 * stanza-id while the original client id moved into `wireIds`. Strict
 * equality on `m.id` would miss those messages.
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
  const index = timeline.findIndex((m) => m.isSelf && matchMessageId(m, messageId));
  if (index < 0) return timeline;
  const current = timeline[index]!;
  const nextStatus = applyDeliveryEvent(current.deliveryStatus, event);
  if (nextStatus === current.deliveryStatus) return timeline;
  const next = timeline.slice();
  next[index] = { ...current, deliveryStatus: nextStatus };
  return next;
}

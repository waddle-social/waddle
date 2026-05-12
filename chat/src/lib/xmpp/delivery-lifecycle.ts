import type { DeliveryStatus } from "@/lib/chat-ui";

// Pure DeliveryStatus state machine for the outbound send lifecycle.
//
// Inputs collapse the various transport signals into a single event type:
// - "queued"    — stanza enqueued (XEP-0198 ackable outbound, or local queue)
// - "sending"   — transport picked up the stanza
// - "delivered" — confirmed delivered (XEP-0184 receipt, XEP-0198 SM ack,
//                 or self-echo from MUC reflection)
// - "failed"    — transport gave up (resume failed / no resumable session)
//
// "delivered" means *delivered to the server / reflected by the room*, not
// "read". Read state lives separately on `TimelineMessage.readBy` and is
// driven by XEP-0333 chat markers. These are distinct lifecycles by design:
// a message can be "delivered" but never read, and the displayed marker can
// only move forward independently of any later XEP-0184/0198 signal.
//
// Invariants:
// - "delivered" is terminal — duplicate or stale events MUST NOT downgrade
//   a confirmed delivery. This guards against XEP-0198's documented
//   retransmit-on-resume behavior, late XEP-0184 receipts, and out-of-order
//   self-echo + failure signals.
// - Every other transition is idempotent: applying the same event twice
//   yields the same result the second time.
// - "failed" is NOT terminal: a delayed XEP-0184 receipt or XEP-0198 ack
//   may upgrade a previously-failed message to "delivered", because the
//   transport's failure callback can race a confirmation that's already
//   in flight from the server.

export type DeliveryEvent = "queued" | "sending" | "delivered" | "failed";

export function applyDeliveryEvent(
  current: DeliveryStatus | undefined,
  event: DeliveryEvent,
): DeliveryStatus {
  if (current === "delivered") return "delivered";
  return event;
}

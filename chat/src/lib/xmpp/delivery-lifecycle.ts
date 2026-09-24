import type { DeliveryStatus } from "@/lib/chat-ui";

// Stream acceptance (XEP-0198) is not recipient delivery. A stanza error
// overrides it. A transport failure alone cannot undo an acknowledgement.
// Explicit rejection is terminal for this send; a manual retry uses a new ID.
export type DeliveryEvent = DeliveryStatus;

export function applyDeliveryEvent(
  current: DeliveryStatus | undefined,
  event: DeliveryEvent,
): DeliveryStatus {
  if (current === "rejected" || event === "rejected") return "rejected";
  if (current === "delivered") return "delivered";
  return event;
}

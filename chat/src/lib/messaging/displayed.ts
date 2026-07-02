import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageIndexById } from "@/lib/message-ids";

// XEP-0333 displayed-marker merge, shared verbatim by the channel and DM
// pipelines (no divergences for this concern).

/**
 * Records a `<displayed/>` chat marker (XEP-0333) for `reader` on the
 * target row. Cumulative — all earlier messages are implicitly displayed
 * for that reader too, but only the exact target is mutated since the
 * timeline carries `readBy` per message.
 *
 * Returns the updated timeline, or `null` on target miss / already-marked
 * so callers can skip the ref reassignment (avoids allocating on every
 * inbound XEP-0333 marker).
 */
export function applyDisplayedMarker(
  messages: TimelineMessage[],
  messageId: string,
  reader: string,
): TimelineMessage[] | null {
  const index = findMessageIndexById(messages, messageId);
  if (index < 0) return null;
  const current = messages[index]!;
  const readBy = current.readBy ? [...current.readBy] : [];
  if (readBy.includes(reader)) return null;
  readBy.push(reader);
  const next = messages.slice();
  next[index] = { ...current, readBy };
  return next;
}

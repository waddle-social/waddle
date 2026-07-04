import type { TimelineMessage } from "@/lib/chat-ui";

// XEP-0444 reaction merge mechanics shared by the channel and DM
// pipelines. Replace semantics (the sender's new set replaces their old
// one) are enforced by the per-pipeline `reactedRow` bookkeeping; the
// shared part is the target-miss short-circuit and the immutable row
// swap. Divergences stay at the call sites as a `ReactionPolicy`:
//   * target resolution (divergence 4): channel resolves strictly via the
//     room-assigned stanza-id mirror (`reactionTargetId`); DM resolves via
//     the row's primary id plus XEP-0359 wire aliases;
//   * sender bookkeeping (divergence 5): channel keeps the occupant-keyed
//     `reactionSenders` map; DM keeps only the nick-keyed aggregate.

export interface ReactionEvent {
  targetId: string;
  nick: string;
  emojis: string[];
  /** Channel: MUC occupant JID; DM: same as `nick`. */
  senderId: string;
}

export interface ReactionPolicy {
  /** Divergence 4: how the target row is located. */
  findTargetIndex: (messages: TimelineMessage[], targetId: string) => number;
  /** Divergence 5: how the sender's reaction set is rewritten on the row. */
  reactedRow: (target: TimelineMessage, event: ReactionEvent) => TimelineMessage;
}

/**
 * Applies a live XEP-0444 reaction update to the timeline. Returns the
 * updated timeline, or `null` on target miss so callers can skip the ref
 * reassignment.
 */
export function applyReactionUpdate(
  messages: TimelineMessage[],
  event: ReactionEvent,
  policy: ReactionPolicy,
): TimelineMessage[] | null {
  const index = policy.findTargetIndex(messages, event.targetId);
  if (index < 0) return null;
  const next = messages.slice();
  next[index] = policy.reactedRow(messages[index]!, event);
  return next;
}

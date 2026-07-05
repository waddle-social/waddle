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
  /** RFC 3339 instant the reaction was emitted (delay stamp for SM/MAM
   * replays). Absent on a live undelayed stanza — those always apply. */
  occurredAt?: string;
}

/**
 * XEP-0444 Business Rules recency check: a delayed reaction SHOULD be
 * accepted only if no NEWER reaction from the same sender was already
 * accepted. `reactionTimes` on the row records the last-applied instant
 * per sender nick (nicks are the stable per-conversation sender key on
 * both the channel and DM paths).
 */
export function isStaleReactionUpdate(
  target: TimelineMessage,
  senderNick: string,
  occurredAt: string | undefined,
): boolean {
  if (!occurredAt) return false;
  const applied = target.reactionTimes?.[senderNick];
  if (!applied) return false;
  const appliedMs = Date.parse(applied);
  const incomingMs = Date.parse(occurredAt);
  return Number.isFinite(appliedMs) && Number.isFinite(incomingMs) && appliedMs > incomingMs;
}

/** Immutably stamp the sender's last-applied reaction instant on the row. */
function withReactionTime(
  row: TimelineMessage,
  senderNick: string,
  occurredAt: string | undefined,
): TimelineMessage {
  if (!occurredAt) return row;
  return { ...row, reactionTimes: { ...(row.reactionTimes ?? {}), [senderNick]: occurredAt } };
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
  const target = messages[index]!;
  // C5: reject a delayed reaction that is older than one already
  // applied for the same sender (XEP-0444 Business Rules).
  if (isStaleReactionUpdate(target, event.nick, event.occurredAt)) return null;
  const next = messages.slice();
  next[index] = withReactionTime(policy.reactedRow(target, event), event.nick, event.occurredAt);
  return next;
}

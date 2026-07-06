import type { ReactionRecencyStamp, TimelineMessage } from "@/lib/chat-ui";

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
  /** Channel: real bare JID when known, else MUC occupant JID; DM: same
   * as `nick`. */
  senderId: string;
  /** RFC 3339 instant the reaction was emitted (delay stamp for SM/MAM
   * replays). Absent on a live undelayed stanza — those always apply. */
  occurredAt?: string;
}

/** Inputs to the XEP-0444 recency gate. */
export interface ReactionRecencyProbe {
  /** Recency key — MUST be the same identity the replacement bookkeeping
   * keys by (`senderId`: real bare JID / occupant JID in MUCs, nick in
   * DMs). Nicks are NOT stable in MUCs — a rename must not reset the
   * gate, and nick reuse by another occupant must not trip it. */
  senderKey: string;
  /** Display nick, used only for the legacy nick-keyed reaction fallback. */
  nick: string;
  emojis: string[];
  occurredAt?: string;
}

/**
 * XEP-0444 Business Rules recency check: a delayed reaction SHOULD be
 * accepted only if no NEWER reaction from the same sender was already
 * accepted. `reactionTimes` on the row records the last-applied recency
 * stamp per sender key (see `ReactionRecencyProbe.senderKey`).
 */
export function isStaleReactionUpdate(
  target: TimelineMessage,
  probe: ReactionRecencyProbe,
): boolean {
  if (!probe.occurredAt) return false;
  const applied = target.reactionTimes?.[probe.senderKey];
  if (applied === undefined) return false;
  const incomingMs = Date.parse(probe.occurredAt);
  if (!Number.isFinite(incomingMs)) return false;
  if (typeof applied !== "string") {
    // Local-domain stamp (live undelayed reaction). Never compare the
    // local clock against server delay stamps — a client clock ahead by
    // Δ would drop every genuinely newer reaction replayed within Δ.
    // The recorded ordering fact is "applied after wire-stamp X": a
    // replay stamped at or before X belongs to the superseded past;
    // anything after X is genuinely newer and applies.
    if (applied.appliedAfterWire === null) return false;
    const anchorMs = Date.parse(applied.appliedAfterWire);
    return Number.isFinite(anchorMs) && incomingMs <= anchorMs;
  }
  const appliedMs = Date.parse(applied);
  if (!Number.isFinite(appliedMs)) return false;
  if (appliedMs !== incomingMs) return appliedMs > incomingMs;
  // Equal-stamp tie: the server truncates XEP-0203 delay stamps to whole
  // seconds, so a same-second toggle replays as two stanzas with
  // identical stamps. XEP-0444 rejects only if a strictly NEWER reaction
  // was already accepted — on a tie, in-order delivery makes the
  // later-processed update the newer one, so a different set applies.
  // Only an exact re-delivery (same set) is a stale idempotent no-op.
  return sameEmojiSet(appliedEmojiSet(target, probe.senderKey, probe.nick), probe.emojis);
}

function sameEmojiSet(a: string[], b: string[]): boolean {
  const aSet = new Set(a);
  const bSet = new Set(b);
  return aSet.size === bSet.size && [...aSet].every((emoji) => bSet.has(emoji));
}

/** The sender's currently applied emoji set on the row: prefers the
 * senderId-keyed `reactionSenders` bookkeeping (channel rows), falling
 * back to the nick-keyed aggregate (DM rows / legacy channel rows). The
 * key matching mirrors `removeSenderReactions`. */
function appliedEmojiSet(target: TimelineMessage, senderKey: string, nick: string): string[] {
  const senders = target.reactionSenders;
  if (senders && Object.keys(senders).length > 0) {
    return Object.entries(senders)
      .filter(([, bySender]) =>
        Object.keys(bySender).some((id) => id === senderKey || id === nick || id.endsWith(`/${nick}`)))
      .map(([emoji]) => emoji);
  }
  return Object.entries(target.reactions ?? {})
    .filter(([, nicks]) => nicks.includes(nick))
    .map(([emoji]) => emoji);
}

/** Immutably stamp the sender's last-applied reaction recency on the row. */
function withReactionTime(
  row: TimelineMessage,
  senderKey: string,
  occurredAt: string | undefined,
): TimelineMessage {
  // A live undelayed reaction carries no wire instant, but it MUST still
  // advance the sender's recency — otherwise a later re-delivery of an
  // older delayed stanza passes the stale check and clobbers it. Never
  // stamp the local clock (mixed clock domains — see
  // `isStaleReactionUpdate`); record the local-domain ordering fact
  // instead: applied after the newest wire stamp seen for this sender.
  const stamp: ReactionRecencyStamp = occurredAt ?? localDomainStamp(row.reactionTimes?.[senderKey]);
  return { ...row, reactionTimes: { ...(row.reactionTimes ?? {}), [senderKey]: stamp } };
}

function localDomainStamp(applied: ReactionRecencyStamp | undefined): ReactionRecencyStamp {
  if (typeof applied === "string") return { appliedAfterWire: applied };
  return { appliedAfterWire: applied?.appliedAfterWire ?? null };
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
  const probe: ReactionRecencyProbe = {
    senderKey: event.senderId,
    nick: event.nick,
    emojis: event.emojis,
    ...(event.occurredAt ? { occurredAt: event.occurredAt } : {}),
  };
  if (isStaleReactionUpdate(target, probe)) return null;
  const next = messages.slice();
  next[index] = withReactionTime(policy.reactedRow(target, event), event.senderId, event.occurredAt);
  return next;
}

import type { TimelineMessage } from "@/lib/chat-ui";
import type { SafetyScoresFastening } from "./types";

// Applies a safety-scores fastening to the timeline row it targets.
// XEP-0422 §Replacing / §Removing semantics: a replace overwrites any
// earlier scores on the row, a clear removes them. The fastening sender is
// gated before this point (see `./sender`).

/**
 * How `apply-to@id` may resolve:
 * - `room`: the room-assigned XEP-0359 stanza-id wins; otherwise the id
 *   may name a sender-chosen alias (origin-id / message id — what
 *   XEP-0422 §Wrapped Payloads literally names), but only when exactly
 *   one loaded row carries it, so a colliding id never lands on the wrong
 *   message.
 * - `dm`: only this account's own archive stanza-id. The DM fastening
 *   names no conversation, so a sender-chosen alias could match a row in
 *   whichever conversation happens to be open.
 */
export type SafetyScoresTargetScope = "room" | "dm";

function findTargetIndex(
  timeline: readonly TimelineMessage[],
  targetId: string,
  scope: SafetyScoresTargetScope,
): number {
  const byStanzaId = timeline.findIndex((message) => message.stanzaId === targetId);
  if (byStanzaId >= 0 || scope === "dm") return byStanzaId;
  const aliasMatches = timeline.flatMap((message, index) =>
    message.id === targetId
    || message.replyableId === targetId
    || message.reactionTargetId === targetId
    || message.wireIds?.includes(targetId)
      ? [index]
      : []
  );
  return aliasMatches.length === 1 ? aliasMatches[0]! : -1;
}

/** An update stamped before the one already applied must not win
 * (archive pages load newest-first, so an older page can replay an older
 * fastening after a newer one landed). */
function isStale(row: TimelineMessage, at: string | undefined): boolean {
  if (!at || !row.safetyScoresAt) return false;
  const incoming = Date.parse(at);
  const applied = Date.parse(row.safetyScoresAt);
  return Number.isFinite(incoming) && Number.isFinite(applied) && incoming < applied;
}

/** Index of the row this fastening should update, or -1 when the target
 * is not loaded, ambiguous, or already carries a newer fastening. */
export function safetyScoresTargetIndex(
  timeline: readonly TimelineMessage[],
  fastening: SafetyScoresFastening,
  scope: SafetyScoresTargetScope,
  at?: string,
): number {
  const index = findTargetIndex(timeline, fastening.targetId, scope);
  if (index < 0 || isStale(timeline[index]!, at)) return -1;
  return index;
}

export function withSafetyScores(
  row: TimelineMessage,
  fastening: SafetyScoresFastening,
  at?: string,
): TimelineMessage {
  const next: TimelineMessage = { ...row };
  if (fastening.kind === "replace") next.safetyScores = fastening.scores;
  else delete next.safetyScores;
  if (at) next.safetyScoresAt = at;
  return next;
}

/** Immutable apply; `null` when nothing changes. */
export function applySafetyScoresFastening(
  timeline: readonly TimelineMessage[],
  fastening: SafetyScoresFastening,
  scope: SafetyScoresTargetScope,
  at?: string,
): TimelineMessage[] | null {
  const index = safetyScoresTargetIndex(timeline, fastening, scope, at);
  if (index < 0) return null;
  const next = timeline.slice();
  next[index] = withSafetyScores(timeline[index]!, fastening, at);
  return next;
}

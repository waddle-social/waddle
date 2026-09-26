import type { TimelineMessage } from "@/lib/chat-ui";
import { bareJidKey } from "@/lib/xmpp/jid";
import type { SafetyScoresFastening } from "./types";

// Applies a safety-scores fastening to the timeline row it targets.
// The fastening sender is gated before this point (see `./sender`).

function findTargetIndex(
  timeline: readonly TimelineMessage[],
  fastening: SafetyScoresFastening,
): number {
  const matches = timeline.flatMap((message, index) =>
    message.stanzaId === fastening.targetStanzaId
    && !!message.stanzaIdBy
    && bareJidKey(message.stanzaIdBy) === bareJidKey(fastening.targetStanzaBy)
    && message.originId === fastening.targetOriginId
    && (message.sourceRevisionId ?? message.stanzaId) === fastening.sourceRevisionId
    && !message.isRetracted ? [index] : []
  );
  return matches.length === 1 ? matches[0]! : -1;
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
  at?: string,
): number {
  const index = findTargetIndex(timeline, fastening);
  if (index < 0 || isStale(timeline[index]!, at)) return -1;
  return index;
}

export function withSafetyScores(
  row: TimelineMessage,
  fastening: SafetyScoresFastening,
  at?: string,
): TimelineMessage {
  const next: TimelineMessage = { ...row };
  next.safetyScores = fastening.scores;
  if (at) next.safetyScoresAt = at;
  return next;
}

/** Immutable apply; `null` when nothing changes. */
export function applySafetyScoresFastening(
  timeline: readonly TimelineMessage[],
  fastening: SafetyScoresFastening,
  at?: string,
): TimelineMessage[] | null {
  const index = safetyScoresTargetIndex(timeline, fastening, at);
  if (index < 0) return null;
  const next = timeline.slice();
  next[index] = withSafetyScores(timeline[index]!, fastening, at);
  return next;
}

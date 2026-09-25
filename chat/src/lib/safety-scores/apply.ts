import type { TimelineMessage } from "@/lib/chat-ui";
import type { SafetyScoresFastening } from "./types";

// Applies a safety-scores fastening to the timeline row it targets.
// XEP-0422 §Replacing / §Removing semantics: a replace overwrites any
// earlier scores on the row, a clear removes them. The fastening sender is
// gated before this point (see `./sender`).

/**
 * Resolve `apply-to@id`. The server-assigned XEP-0359 stanza-id is
 * globally unique within the archive and wins; otherwise fall back to the
 * row's other XEP-0359 identities (origin-id / message id aliases), which
 * is what XEP-0422 §Wrapped Payloads literally names.
 */
export function findSafetyScoresTargetIndex(
  timeline: readonly TimelineMessage[],
  targetId: string,
): number {
  const byStanzaId = timeline.findIndex((message) => message.stanzaId === targetId);
  if (byStanzaId >= 0) return byStanzaId;
  return timeline.findIndex((message) =>
    message.id === targetId
    || message.replyableId === targetId
    || message.reactionTargetId === targetId
    || !!message.wireIds?.includes(targetId)
  );
}

export function withSafetyScores(
  row: TimelineMessage,
  fastening: SafetyScoresFastening,
): TimelineMessage {
  const next: TimelineMessage = { ...row };
  if (fastening.kind === "replace") next.safetyScores = fastening.scores;
  else delete next.safetyScores;
  return next;
}

/** Immutable apply; `null` when the target is not in this timeline. */
export function applySafetyScoresFastening(
  timeline: readonly TimelineMessage[],
  fastening: SafetyScoresFastening,
): TimelineMessage[] | null {
  const index = findSafetyScoresTargetIndex(timeline, fastening.targetId);
  if (index < 0) return null;
  const next = timeline.slice();
  next[index] = withSafetyScores(timeline[index]!, fastening);
  return next;
}

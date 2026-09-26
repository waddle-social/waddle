import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageIndexById } from "@/lib/message-ids";
import { assignCorrectionFields, type CorrectionPayload } from "@/lib/messaging/correction";
import { applySafetyScoresFastening } from "@/lib/safety-scores/apply";
import type { SafetyScoresFastening } from "@/lib/safety-scores/types";

type CorrectionSender = { authorJid: string; authorRealJid?: string };
export type PendingChannelCorrection = {
  targetId: string;
  correctionSender: CorrectionSender;
  payload: CorrectionPayload;
};

type QueuedCorrection = PendingChannelCorrection & { archived: boolean };
const MAX_PENDING = 100;

/** Shared by live delivery and archive paging for the current room/session. */
export class ChannelPendingUpdates {
  private corrections: QueuedCorrection[] = [];
  private scores: { fastening: SafetyScoresFastening; at?: string }[] = [];

  clear(): void {
    this.corrections = [];
    this.scores = [];
  }

  addCorrection(correction: PendingChannelCorrection): void {
    this.corrections.unshift({ ...correction, archived: false });
    this.corrections.length = Math.min(this.corrections.length, MAX_PENDING);
  }

  addArchivedCorrections(corrections: PendingChannelCorrection[]): void {
    // Each MAM page is chronological; earlier pages are loaded afterward.
    // Retain newest-first order even when archive timestamps are identical.
    this.corrections.push(...corrections.toReversed().map((correction) => ({ ...correction, archived: true })));
    this.corrections.length = Math.min(this.corrections.length, MAX_PENDING);
  }

  addScores(fastening: SafetyScoresFastening, at?: string): void {
    if (this.scores.length === MAX_PENDING) this.scores.shift();
    this.scores.push({ fastening, at });
  }

  applyCorrections(
    timeline: TimelineMessage[],
    senderMatches: (target: TimelineMessage, sender: CorrectionSender) => boolean,
  ): TimelineMessage[] {
    let next = timeline;
    const applied = new Set<number>();
    this.corrections.sort((left, right) => {
      const leftAt = Date.parse(left.payload.sourceRevisionAt ?? "");
      const rightAt = Date.parse(right.payload.sourceRevisionAt ?? "");
      return Number.isFinite(leftAt) && Number.isFinite(rightAt) ? rightAt - leftAt : 0;
    });
    this.corrections = this.corrections.filter((update) => {
      const policy = { senderMatches: (target: TimelineMessage) => senderMatches(target, update.correctionSender) };
      const index = findMessageIndexById(next, update.targetId, policy.senderMatches);
      if (index < 0) return true;
      const target = next[index]!;
      const incomingAt = Date.parse(update.payload.sourceRevisionAt ?? "");
      const currentAt = Date.parse(target.sourceRevisionAt ?? "");
      const stale = applied.has(index) || Number.isFinite(incomingAt) && Number.isFinite(currentAt)
        && (incomingAt < currentAt || (update.archived && incomingAt === currentAt
          && (target.sourceRevisionId ?? "") !== (update.payload.sourceRevisionId ?? "")));
      if (!stale && !target.isRetracted) {
        const updated = { ...target };
        assignCorrectionFields(updated, {
          ...update.payload,
          body: update.archived ? update.payload.body : update.payload.body.trim(),
        });
        next = next.slice();
        next[index] = updated;
        applied.add(index);
      }
      return false;
    });
    return next;
  }

  applyScores(timeline: TimelineMessage[]): TimelineMessage[] {
    let next = timeline;
    this.scores = this.scores.filter(({ fastening, at }) => {
      const applied = applySafetyScoresFastening(next, fastening, at);
      if (!applied) return true;
      next = applied;
      return false;
    });
    return next;
  }
}

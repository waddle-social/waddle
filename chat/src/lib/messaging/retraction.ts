import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageIndexById } from "@/lib/message-ids";

// XEP-0424 retraction merge mechanics shared by the channel and DM
// pipelines. The divergent parts stay at the call sites as a
// `RetractionPolicy`:
//   * target gating (divergence 1): the channel refuses when the target
//     row's XEP-0359 `replyableId` differs from the retraction id; the DM
//     side retracts any id-resolvable row;
//   * XEP-0425 service moderation (divergence 2): channel-only;
//   * sender identity (divergence 3): MUC occupant JID (+ optional
//     real-JID compare) vs bare `fromJid`.

export interface RetractionEvent {
  retractsId: string;
  /** XEP-0424 id of the retraction stanza itself, kept on the tombstone. */
  retractionId?: string;
}

export interface RetractionPolicy {
  /** Divergences 1–3: target/sender gating, incl. channel-only XEP-0425. */
  canRetract: (target: TimelineMessage) => boolean;
}

/**
 * XEP-0424 tombstone transform: strips every content-bearing field and
 * marks the row retracted. Shared verbatim by both pipelines.
 */
export function retractTimelineMessage(
  existing: TimelineMessage,
  retractionId?: string,
): TimelineMessage {
  const next: TimelineMessage = {
    ...existing,
    body: "",
    isRetracted: true,
    ...(retractionId ? { retractionId } : {}),
  };
  delete next.markup;
  delete next.references;
  delete next.sharedFiles;
  delete next.linkPreviews;
  delete next.extensionAnnotations;
  delete next.extensionBodyFallback;
  delete next.isSticker;
  delete next.mentions;
  delete next.broadcastMention;
  delete next.forumPostKind;
  delete next.forumTitle;
  delete next.forumThreadTitle;
  return next;
}

/**
 * Applies a live XEP-0424 retraction to the timeline. Returns the updated
 * timeline, or `null` on target miss / policy refusal so callers can skip
 * the ref reassignment (a spoofed retraction must not tombstone someone
 * else's message).
 */
export function applyRetraction(
  messages: TimelineMessage[],
  event: RetractionEvent,
  policy: RetractionPolicy,
): TimelineMessage[] | null {
  const index = findMessageIndexById(messages, event.retractsId);
  if (index < 0) return null;
  const target = messages[index]!;
  if (!policy.canRetract(target)) return null;
  const next = messages.slice();
  next[index] = retractTimelineMessage(target, event.retractionId);
  return next;
}

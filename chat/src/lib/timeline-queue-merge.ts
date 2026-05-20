import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import { compareTimelineMessages } from "@/lib/timeline-timestamps";

/**
 * Append locally-queued (still-pending) outbound messages onto a
 * timeline that just arrived from MAM, then re-sort through the
 * canonical total-order comparator.
 *
 * Shared between DM and channel composables — both call this from
 * four sites each (initial load, load-older, restore-from-cursor,
 * stale-cursor recovery). The single source of truth for queue+sort
 * ordering also makes future invariant changes (e.g. tightening the
 * pending-bucket rule in `compareTimelineMessages`) effective
 * everywhere at once instead of needing parallel edits.
 *
 * Concat-without-sort is correct only when wall clocks don't drift;
 * a still-pending optimistic row whose client `createdAt` sits
 * inside the queued range would otherwise mis-interleave. The
 * canonical comparator (with its pending-bucket-last rule) handles
 * this regardless of clock drift.
 *
 * `postProcess` lets the channel side re-apply `applyForumContext`
 * to the merged result — the only kind-specific step. DM passes
 * `undefined`.
 */
export function mergeQueuedIntoTimeline(
  timeline: TimelineMessage[],
  queued: TimelineMessage[],
  postProcess?: (sorted: TimelineMessage[]) => TimelineMessage[],
): TimelineMessage[] {
  const fresh = queued.filter((message) => !findMessageById(timeline, message.id));
  if (fresh.length === 0) return timeline;
  const sorted = [...timeline, ...fresh].sort(compareTimelineMessages);
  return postProcess ? postProcess(sorted) : sorted;
}

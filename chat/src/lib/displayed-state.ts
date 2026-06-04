import type { TimelineMessage } from "@/lib/chat-ui";
import type { MdsDisplayedState } from "@/lib/last-seen-store";

function isUnreadEligible(message: TimelineMessage): boolean {
  return !message.isSelf && !message.isRetracted;
}

function nextUnreadEligibleIdAfter(
  timeline: ReadonlyArray<TimelineMessage>,
  index: number,
): string | null {
  return timeline.slice(index + 1).find(isUnreadEligible)?.id ?? null;
}

function displayedStateIndex(
  timeline: ReadonlyArray<TimelineMessage>,
  displayed: MdsDisplayedState | null | undefined,
): number {
  const stanzaId = displayed?.stanzaId.trim();
  const stanzaIdBy = displayed?.stanzaIdBy.trim();
  if (!stanzaId || !stanzaIdBy) return -1;
  return timeline.findIndex(
    (message) => message.stanzaId === stanzaId && message.stanzaIdBy === stanzaIdBy,
  );
}

export function firstUnseenIdFromUnreadCount(
  timeline: ReadonlyArray<TimelineMessage>,
  unreadAtLoad: number,
): string | null {
  const unreadEligible = timeline.filter(isUnreadEligible);
  return unreadAtLoad > 0 && unreadEligible.length >= unreadAtLoad
    ? unreadEligible[unreadEligible.length - unreadAtLoad]?.id ?? null
    : null;
}

export function firstUnseenIdAfterDisplayedState(
  timeline: ReadonlyArray<TimelineMessage>,
  currentFirstUnseenId: string | null,
  displayed: MdsDisplayedState | null | undefined,
): string | null {
  const displayedAt = displayedStateIndex(timeline, displayed);
  if (displayedAt < 0 || currentFirstUnseenId === null) return currentFirstUnseenId;

  const currentAt = timeline.findIndex((message) => message.id === currentFirstUnseenId);
  if (currentAt < 0 || displayedAt < currentAt) return currentFirstUnseenId;
  return nextUnreadEligibleIdAfter(timeline, displayedAt);
}

export function displayedStateCanAdvance(
  timeline: ReadonlyArray<TimelineMessage>,
  current: MdsDisplayedState | null | undefined,
  incoming: MdsDisplayedState,
): boolean {
  const incomingAt = displayedStateIndex(timeline, incoming);
  if (incomingAt < 0) return false;
  const currentAt = displayedStateIndex(timeline, current);
  return currentAt < 0 || incomingAt >= currentAt;
}

export function latestDisplayedStateOnTimeline(
  timeline: ReadonlyArray<TimelineMessage>,
  candidates: ReadonlyArray<MdsDisplayedState>,
): MdsDisplayedState | null {
  let latest: { state: MdsDisplayedState; index: number } | null = null;
  for (const state of candidates) {
    const index = displayedStateIndex(timeline, state);
    if (index < 0) continue;
    if (!latest || index > latest.index) latest = { state, index };
  }
  return latest?.state ?? null;
}

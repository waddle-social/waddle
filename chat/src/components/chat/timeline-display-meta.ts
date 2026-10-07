import { timelineRowKey } from "@/lib/timeline-row-key";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { OccupantPresence } from "@/lib/xmpp-client";
import type { MessageThreadIndex } from "@/channels/threads";
import { isSameTimelineDay } from "@/channels/timeline";

// Burst window: same author + < 5 min apart + same day, with no
// intervening other-author message in the rendered order.
const BURST_WINDOW_MS = 5 * 60 * 1000;

export interface MessageDisplayMeta {
  /** Messages rendered as grouped follow-ups (no avatar/meta row). */
  grouped: Set<string>;
  /** Messages preceded by a day divider. */
  dayDivider: Set<string>;
}

export function buildMessageDisplayMeta(list: readonly TimelineMessage[]): MessageDisplayMeta {
  const grouped = new Set<string>();
  const dayDivider = new Set<string>();
  for (let i = 0; i < list.length; i++) {
    const cur = list[i];
    if (!cur) continue;
    const prev = i > 0 ? list[i - 1] : null;
    if (!prev) continue;
    const sameDay = isSameTimelineDay(prev.createdAt, cur.createdAt);
    if (!sameDay) dayDivider.add(timelineRowKey(cur));
    if (
      sameDay
      && prev.author === cur.author
      && Math.abs(new Date(cur.createdAt).getTime() - new Date(prev.createdAt).getTime()) < BURST_WINDOW_MS
    ) {
      grouped.add(timelineRowKey(cur));
    }
  }
  return { grouped, dayDivider };
}

const THREAD_CHIP_MAX_PARTICIPANTS = 5;

/**
 * Identity a thread participant is deduplicated by: the resolved real JID
 * when known, so two people who used the same nick stay distinct. An
 * unresolved author falls back to its sender identity (occupant JID or
 * nick); it can never merge with a resolved one, and two unresolved
 * authors sharing a nick would render identical initials anyway.
 */
export function threadParticipantKey(message: TimelineMessage, jid: string | null): string {
  if (jid) return `jid:${jid}`;
  return `unresolved:${message.authorOccupantJid ?? message.authorJid ?? message.author}`;
}

export interface ThreadChipParticipant {
  /** Dedupe identity (see `threadParticipantKey`); stable per participant. */
  key: string;
  nick: string;
  /** Real bare JID behind the author, for the avatar; `null` = initials. */
  jid: string | null;
  presence: OccupantPresence;
}

/**
 * Distinct participants for a thread chip: walk newest→oldest, dedup,
 * keep EVERYONE who replied — including the thread author themselves
 * and the current user. Previously the current user was excluded
 * (logic: "the chip answers who *else* has been talking"), but that
 * made the chip read inconsistently: a stranger's reply showed an
 * avatar, my own reply showed nothing. Capped at
 * THREAD_CHIP_MAX_PARTICIPANTS — MessageCard renders only the first
 * N visibly and shows a "+N" overflow chip.
 */
export function threadChipParticipants(
  threadIndex: MessageThreadIndex,
  messageId: string,
  authorJid: (message: TimelineMessage) => string | null,
  roomPresence: Record<string, OccupantPresence>,
): ThreadChipParticipant[] {
  const entry = threadIndex.get(messageId);
  if (!entry) return [];
  const seen = new Set<string>();
  const ordered: ThreadChipParticipant[] = [];
  const children = entry.directChildren;
  for (let i = children.length - 1; i >= 0; i--) {
    const c = children[i];
    if (!c) continue;
    const jid = authorJid(c);
    const key = threadParticipantKey(c, jid);
    if (seen.has(key)) continue;
    seen.add(key);
    ordered.push({
      key,
      nick: c.author,
      jid,
      presence: roomPresence[c.author] ?? "offline",
    });
    if (ordered.length >= THREAD_CHIP_MAX_PARTICIPANTS) break;
  }
  return ordered;
}

export function threadChipLastReplyAt(
  threadIndex: MessageThreadIndex,
  messageId: string,
): string | undefined {
  const entry = threadIndex.get(messageId);
  return entry?.directChildren.at(-1)?.createdAt;
}

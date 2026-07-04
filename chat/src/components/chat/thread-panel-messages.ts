import type { MessageThreadEntry } from "@/channels/threads";
import type { TimelineMessage } from "@/lib/chat-ui";
import {
  isTopPinnedScrollDirection,
  orderTimelineForScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";

/**
 * Direct replies of the active thread, ordered for the current scroll
 * direction (reversed in social/top-pinned mode).
 */
export function orderThreadChildren(
  entry: MessageThreadEntry | null,
  mode: ScrollDirectionMode,
): TimelineMessage[] {
  return orderTimelineForScrollDirection(entry?.directChildren ?? [], mode);
}

/**
 * Rendered thread list: replies plus the root message. The root is always
 * the oldest message in the thread, so it sits at whichever end of the
 * rendered list represents "oldest" for the active scroll mode: bottom in
 * social/top-pinned mode, top in chat/bottom-pinned mode.
 */
export function orderThreadMessages(
  entry: MessageThreadEntry | null,
  orderedChildren: readonly TimelineMessage[],
  mode: ScrollDirectionMode,
): TimelineMessage[] {
  const root = entry?.root;
  if (!root) return [...orderedChildren];
  return isTopPinnedScrollDirection(mode)
    ? [...orderedChildren, root]
    : [root, ...orderedChildren];
}

/**
 * Replies that belong to the active thread itself (not a nested child
 * thread), in chronological order — the read-tracking scope.
 */
export function chronologicalThreadReplies(
  entry: MessageThreadEntry | null,
  threadId: string | null,
): TimelineMessage[] {
  if (!entry || !threadId) return [];
  return entry.directChildren.filter((message) => message.threadId === threadId);
}

export function newestThreadMessageId(entry: MessageThreadEntry | null): string | null {
  return entry?.directChildren.at(-1)?.id
    ?? entry?.root?.id
    ?? null;
}

/**
 * Older replies paginate at the chronologically-older end of the list. In
 * social mode the root is appended after the loaded replies, so the sentinel
 * has to sit one slot inboard of the trailing root rather than below it.
 */
export function olderRepliesSentinelPosition(
  mode: ScrollDirectionMode,
  hasRoot: boolean,
): "start" | "end" | "before-end" {
  if (mode !== "social") return "start";
  return hasRoot ? "before-end" : "end";
}

/**
 * Message sitting at the live edge for `mode`: the newest rendered message
 * when top-pinned, otherwise the last entry of the rendered list (falling
 * back to the oldest rendered message).
 */
export function threadEdgeMessage(
  orderedChildren: readonly TimelineMessage[],
  orderedMessages: readonly TimelineMessage[],
  root: TimelineMessage | null,
  mode: ScrollDirectionMode,
): TimelineMessage | null {
  const newestRendered = orderedChildren[0] ?? root ?? null;
  const oldestRendered = orderedChildren[orderedChildren.length - 1] ?? root ?? null;
  return isTopPinnedScrollDirection(mode)
    ? newestRendered
    : (orderedMessages[orderedMessages.length - 1] ?? oldestRendered);
}

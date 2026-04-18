import { computed, type Ref } from "vue";
import type { TimelineMessage } from "@/lib/chat-ui";

export interface ThreadEntry {
  threadId: string;
  /** Message whose id === threadId (XEP-0201 convention). null when the root isn't in the loaded window. */
  root: TimelineMessage | null;
  /** Messages whose threadId === this entry's threadId, in chronological order, excluding the root. */
  directChildren: TimelineMessage[];
  /** Messages in this thread plus every nested sub-thread, chronological. */
  allDescendants: TimelineMessage[];
  /** Total reply count across this thread and its sub-threads. */
  count: number;
  /** ISO timestamp of the most recent descendant (or root when empty). */
  lastTs: string;
}

export type ThreadIndex = ReadonlyMap<string, ThreadEntry>;

export interface UseThreadsResult {
  index: Readonly<Ref<ThreadIndex>>;
  getEntry: (threadId: string | null | undefined) => ThreadEntry | undefined;
  hasChildren: (messageId: string) => boolean;
  rootOf: (message: TimelineMessage | null | undefined) => TimelineMessage | null;
}

function byCreatedAt(a: TimelineMessage, b: TimelineMessage): number {
  return a.createdAt.localeCompare(b.createdAt);
}

function buildIndex(messages: readonly TimelineMessage[]): ThreadIndex {
  if (messages.length === 0) return new Map();

  const byThread = new Map<string, TimelineMessage[]>();
  const roots = new Map<string, TimelineMessage>();
  const parentLinks = new Map<string, string>();

  for (const msg of messages) {
    if (!msg.threadId) continue;
    const group = byThread.get(msg.threadId);
    if (group) group.push(msg);
    else byThread.set(msg.threadId, [msg]);
    if (msg.id === msg.threadId) {
      roots.set(msg.threadId, msg);
    }
    if (msg.parentThreadId && !parentLinks.has(msg.threadId)) {
      parentLinks.set(msg.threadId, msg.parentThreadId);
    }
  }

  const entries = new Map<string, ThreadEntry>();
  for (const [threadId, group] of byThread) {
    group.sort(byCreatedAt);
    const root = roots.get(threadId) ?? null;
    const directChildren = group.filter((m) => m.id !== threadId);
    const lastTs = group[group.length - 1]?.createdAt ?? root?.createdAt ?? "";
    entries.set(threadId, {
      threadId,
      root,
      directChildren,
      allDescendants: directChildren.slice(),
      count: directChildren.length,
      lastTs,
    });
  }

  // Roll nested sub-thread descendants into ancestor entries so a parent
  // thread's "N replies" count and allDescendants include messages posted in
  // sub-threads beneath it. Walk parent chain for each thread with a parent.
  for (const [threadId, parentId] of parentLinks) {
    const child = entries.get(threadId);
    if (!child) continue;
    let cursor = parentId;
    const visited = new Set<string>([threadId]);
    while (cursor && !visited.has(cursor)) {
      visited.add(cursor);
      const ancestor = entries.get(cursor);
      if (!ancestor) break;
      for (const msg of child.directChildren) {
        ancestor.allDescendants.push(msg);
      }
      if (child.root) ancestor.allDescendants.push(child.root);
      ancestor.count = ancestor.allDescendants.length;
      if (child.lastTs > ancestor.lastTs) {
        ancestor.lastTs = child.lastTs;
      }
      cursor = parentLinks.get(cursor) ?? "";
    }
  }

  for (const entry of entries.values()) {
    entry.allDescendants.sort(byCreatedAt);
  }

  return entries;
}

/**
 * Derive a thread index from the flat message array. The index groups
 * messages by XEP-0201 thread id, identifies the root (id === threadId),
 * and walks parentThreadId links so ancestor threads see their
 * sub-threads' reply counts.
 */
export function useThreads(messages: Ref<readonly TimelineMessage[]>): UseThreadsResult {
  const index = computed<ThreadIndex>(() => buildIndex(messages.value));

  function getEntry(threadId: string | null | undefined): ThreadEntry | undefined {
    if (!threadId) return undefined;
    return index.value.get(threadId);
  }

  function hasChildren(messageId: string): boolean {
    const entry = index.value.get(messageId);
    return !!entry && entry.count > 0;
  }

  function rootOf(message: TimelineMessage | null | undefined): TimelineMessage | null {
    if (!message) return null;
    if (!message.threadId) return null;
    if (message.id === message.threadId) return message;
    return index.value.get(message.threadId)?.root ?? null;
  }

  return { index, getEntry, hasChildren, rootOf };
}

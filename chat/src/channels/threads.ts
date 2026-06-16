import { computed, type Ref } from "vue";
import type { TimelineMessage } from "@/lib/chat-ui";

export interface MessageThreadEntry {
  threadId: string;
  /**
   * Resolved thread root. Two paths:
   *   - Waddle's implicit-root convention: the message whose id equals the
   *     thread id (root carries no `<thread/>`).
   *   - Standard XEP-0201: the earliest message bearing this thread id.
   * `null` when the root isn't in the loaded window.
   */
  root: TimelineMessage | null;
  /** Messages whose threadId === this entry's threadId, in chronological order, excluding the root. */
  directChildren: TimelineMessage[];
  /** Messages in this thread plus every nested sub-thread, chronological. */
  allDescendants: TimelineMessage[];
  /** Total reply count across this thread and its sub-threads. */
  count: number;
  /** ISO timestamp of the most recent descendant (or root when empty). */
  lastTs: string;
  /** XEP-0201 parent thread id when this entry is a child thread. */
  parentThreadId?: string;
}

export type MessageThreadIndex = ReadonlyMap<string, MessageThreadEntry>;

interface UseThreadsResult {
  index: Readonly<Ref<MessageThreadIndex>>;
  getEntry: (threadId: string | null | undefined) => MessageThreadEntry | undefined;
  /**
   * Like `getEntry`, but synthesises an empty entry rooted at the matching
   * message when no replies exist yet. Use this for the thread panel so
   * opening an empty sub-thread renders the root and composer instead of
   * "Loading…".
   */
  resolveEntry: (threadId: string | null | undefined) => MessageThreadEntry | undefined;
  hasChildren: (messageId: string) => boolean;
  rootOf: (message: TimelineMessage | null | undefined) => TimelineMessage | null;
}

function byCreatedAt(a: TimelineMessage, b: TimelineMessage): number {
  return a.createdAt.localeCompare(b.createdAt);
}

function buildIndex(messages: readonly TimelineMessage[]): MessageThreadIndex {
  if (messages.length === 0) return new Map();

  const byId = new Map<string, TimelineMessage>();
  const byThread = new Map<string, TimelineMessage[]>();
  const parentLinks = new Map<string, string>();

  for (const msg of messages) {
    byId.set(msg.id, msg);
    if (!msg.threadId) continue;
    const group = byThread.get(msg.threadId);
    if (group) group.push(msg);
    else byThread.set(msg.threadId, [msg]);
    if (msg.parentThreadId && !parentLinks.has(msg.threadId)) {
      parentLinks.set(msg.threadId, msg.parentThreadId);
    }
  }

  const entries = new Map<string, MessageThreadEntry>();
  for (const [threadId, group] of byThread) {
    group.sort(byCreatedAt);
    // Resolve the thread root via two paths, in order:
    //   1. Waddle's implicit-root convention (`root.id === threadId`, no
    //      `<thread/>` on the root): the loaded message id matches the thread
    //      id directly.
    //   2. Standard XEP-0201, where the thread id is an arbitrary identifier
    //      decoupled from any message id and the root carries
    //      `<thread>threadId</thread>` itself: the earliest message bearing
    //      this thread id that is *not* a reply to another member of the
    //      thread (or to the implicit-root id) is the root candidate.
    // The fallback chain leaves the root null when only XEP-0461 children of
    // an unloaded root are visible — never promote a known reply to a root.
    const groupMembers = new Set(group.map((m) => m.id));
    const standardRoot = group.find((m) => {
      const replyId = m.replyTo?.id;
      if (!replyId) return true;
      if (replyId === threadId) return false;
      return !groupMembers.has(replyId);
    }) ?? null;
    const root = byId.get(threadId) ?? standardRoot;
    const directChildren = root ? group.filter((m) => m.id !== root.id) : group.slice();
    const lastTs = group[group.length - 1]?.createdAt ?? root?.createdAt ?? "";
    const parentThreadId = parentLinks.get(threadId);
    entries.set(threadId, {
      threadId,
      root,
      directChildren,
      allDescendants: directChildren.slice(),
      count: directChildren.length,
      lastTs,
      ...(parentThreadId ? { parentThreadId } : {}),
    });
  }

  // Roll nested sub-thread descendants into ancestor entries so a parent
  // thread's "N replies" count and allDescendants include messages posted in
  // sub-threads beneath it. The sub-thread root (e.g. `c1`) is typically
  // already a direct child of its ancestor outer thread, so de-dupe by id
  // to avoid inflating counts.
  const seenPerEntry = new Map<string, Set<string>>();
  for (const [threadId, entry] of entries) {
    seenPerEntry.set(threadId, new Set(entry.allDescendants.map((m) => m.id)));
  }
  for (const [threadId, parentId] of parentLinks) {
    const child = entries.get(threadId);
    if (!child) continue;
    let cursor = parentId;
    const visited = new Set<string>([threadId]);
    while (cursor && !visited.has(cursor)) {
      visited.add(cursor);
      const ancestor = entries.get(cursor);
      if (!ancestor) break;
      const seen = seenPerEntry.get(cursor)!;
      const pushUnique = (msg: TimelineMessage) => {
        if (seen.has(msg.id)) return;
        seen.add(msg.id);
        ancestor.allDescendants.push(msg);
      };
      for (const msg of child.directChildren) pushUnique(msg);
      if (child.root) pushUnique(child.root);
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
 * Derive a thread index from the flat message array. Messages with a
 * `<thread>` child (replies) are grouped by their thread id; the root is
 * resolved by matching the thread id against any loaded message's id.
 * Walks parentThreadId links so ancestor threads see their sub-threads'
 * reply counts.
 */
export function useMessageThreads(messages: Ref<readonly TimelineMessage[]>): UseThreadsResult {
  const index = computed<MessageThreadIndex>(() => buildIndex(messages.value));
  const byId = computed<ReadonlyMap<string, TimelineMessage>>(() => {
    const map = new Map<string, TimelineMessage>();
    for (const m of messages.value) map.set(m.id, m);
    return map;
  });

  function getEntry(threadId: string | null | undefined): MessageThreadEntry | undefined {
    if (!threadId) return undefined;
    return index.value.get(threadId);
  }

  function resolveEntry(threadId: string | null | undefined): MessageThreadEntry | undefined {
    if (!threadId) return undefined;
    const existing = index.value.get(threadId);
    if (existing) return existing;
    const root = byId.value.get(threadId);
    if (!root) return undefined;
    return {
      threadId,
      root,
      directChildren: [],
      allDescendants: [],
      count: 0,
      lastTs: root.createdAt,
    };
  }

  function hasChildren(messageId: string): boolean {
    const entry = index.value.get(messageId);
    return !!entry && entry.count > 0;
  }

  function rootOf(message: TimelineMessage | null | undefined): TimelineMessage | null {
    if (!message) return null;
    if (!message.threadId) return null;
    return index.value.get(message.threadId)?.root ?? null;
  }

  return { index, getEntry, resolveEntry, hasChildren, rootOf };
}

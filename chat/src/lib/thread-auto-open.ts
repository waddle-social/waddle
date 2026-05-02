import type { TimelineMessage } from "./chat-ui";

/**
 * Returns the threadId of the first thread that should auto-open, or undefined
 * if none qualifies.
 *
 * @param candidates - Messages to scan for a triggering reply (typically only
 *   newly-arrived messages, to avoid re-opening threads from history).
 * @param allMessages - Full message list used to look up the thread root.
 * @param alreadyOpened - Thread IDs already auto-opened this session.
 */
export function findThreadToAutoOpen(
  candidates: readonly TimelineMessage[],
  allMessages: readonly TimelineMessage[],
  alreadyOpened: ReadonlySet<string>,
): string | undefined {
  const byAnyId = new Map<string, TimelineMessage>();
  for (const m of allMessages) {
    byAnyId.set(m.id, m);
    for (const wireId of m.wireIds ?? []) {
      byAnyId.set(wireId, m);
    }
  }
  for (const msg of candidates) {
    if (!msg.threadId || msg.threadId === msg.id) continue;
    const root = byAnyId.get(msg.threadId);
    if (!root?.isSelf) continue;
    if (alreadyOpened.has(root.id)) continue;
    return root.id;
  }
  return undefined;
}

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
  for (const msg of candidates) {
    if (!msg.threadId || msg.threadId === msg.id) continue;
    if (alreadyOpened.has(msg.threadId)) continue;
    const root = allMessages.find((m) => m.id === msg.threadId);
    if (!root?.isSelf) continue;
    return msg.threadId;
  }
  return undefined;
}

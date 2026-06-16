import type { MessageThreadIndex } from "@/channels/threads";
import type { TimelineMessage } from "@/lib/chat-ui";

export interface ReplyChildThreadTarget {
  threadId: string;
  count: number;
}

function threadTargetAliases(message: TimelineMessage): string[] {
  const aliases = new Set<string>();
  for (const id of [message.id, message.replyableId, ...(message.wireIds ?? [])]) {
    const trimmed = id?.trim();
    if (trimmed) aliases.add(trimmed);
  }
  return [...aliases];
}

export function buildReplyChildThreadTargets(
  threadIndex: MessageThreadIndex,
  parentThreadId: string | null | undefined,
): ReadonlyMap<string, ReplyChildThreadTarget> {
  const targets = new Map<string, ReplyChildThreadTarget>();
  if (!parentThreadId) return targets;

  for (const entry of threadIndex.values()) {
    if (entry.threadId === parentThreadId || entry.parentThreadId !== parentThreadId) continue;

    const root = entry.root;
    if (root?.threadId === parentThreadId) {
      targets.set(root.id, { threadId: entry.threadId, count: entry.count });
      continue;
    }

    const anchorId = root?.replyTo?.id ?? entry.directChildren.find((message) => message.replyTo?.id)?.replyTo?.id;
    if (!anchorId) continue;
    targets.set(anchorId, {
      threadId: entry.threadId,
      count: entry.count + (root ? 1 : 0),
    });
  }

  return targets;
}

export function resolveReplyChildThreadTarget(
  targets: ReadonlyMap<string, ReplyChildThreadTarget>,
  message: TimelineMessage,
): ReplyChildThreadTarget {
  for (const id of threadTargetAliases(message)) {
    const target = targets.get(id);
    if (target) return target;
  }
  return { threadId: message.id, count: 0 };
}

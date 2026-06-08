import type { TimelineMessage } from "@/lib/chat-ui";

export type DmThreadRef = {
  id: string;
  parent?: string;
};

function nonBlank(value: string | null | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

export function dmThreadRefFromMessage(
  message: TimelineMessage | null | undefined,
): DmThreadRef | undefined {
  const id = nonBlank(message?.threadId);
  if (!id) return undefined;
  const parent = nonBlank(message?.parentThreadId);
  return parent ? { id, parent } : { id };
}

export function dmThreadRefFromOverride(
  threadOverride: { threadId: string; parentThreadId?: string } | undefined,
): DmThreadRef | undefined {
  const id = nonBlank(threadOverride?.threadId);
  if (!id) return undefined;
  const parent = nonBlank(threadOverride?.parentThreadId);
  return parent ? { id, parent } : { id };
}


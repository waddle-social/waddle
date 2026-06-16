export interface ThreadActionMessage {
  id: string;
  threadId?: string | null;
}

export function resolveThreadActionTarget(
  message: ThreadActionMessage,
  overrideThreadId?: string | null,
): string {
  return overrideThreadId ?? message.threadId ?? message.id;
}

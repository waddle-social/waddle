import type { TimelineMessage } from "@/lib/chat-ui";

export function callThreadAnchorLabel(message: Pick<TimelineMessage, "body" | "author" | "callThread">): string {
  return message.body || `${message.author} started a call`;
}

export function callThreadAnchorThreadId(message: Pick<TimelineMessage, "threadId" | "callThread">): string | null {
  return message.callThread && message.threadId ? message.threadId : null;
}

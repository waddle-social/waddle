import type { TimelineMessage } from "@/lib/chat-ui";

export function callThreadAnchorLabel(message: Pick<TimelineMessage, "body" | "author" | "callThread">): string {
  if (message.callThread?.ended && message.callThread.duration) {
    return `Call ended · ${formatCallThreadDuration(message.callThread.duration)}`;
  }
  return message.body || `${message.author} started a call`;
}

export function callThreadAnchorThreadId(message: Pick<TimelineMessage, "threadId" | "callThread">): string | null {
  return message.callThread && message.threadId ? message.threadId : null;
}

function formatCallThreadDuration(value: string): string {
  const match = /^PT(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)S)?$/.exec(value);
  if (!match) return value;
  const hours = Number(match[1] ?? 0);
  const minutes = Number(match[2] ?? 0);
  const seconds = Number(match[3] ?? 0);
  if (hours > 0) return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
  if (minutes > 0) return `${minutes}m`;
  return `${seconds}s`;
}

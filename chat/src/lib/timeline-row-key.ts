import type { TimelineMessage } from "@/lib/chat-ui";

/** Wire identifiers may collide between distinct room senders. */
export function timelineRowKey(message: TimelineMessage): string {
  return message.rowKey ?? message.id;
}

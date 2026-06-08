import type { CallThreadAnchor } from "@/lib/chat-ui";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

type CallThreadCandidate = {
  threadId?: string;
  callThread?: CallThreadAnchor;
};

/**
 * Resolve the active call's XEP-0201 thread from the current conversation's
 * loaded timeline. The message list is intentionally conversation-scoped by
 * `ContentArea`; timeline messages do not carry a room JID themselves.
 */
export function resolveActiveMucCallThreadId(
  messages: readonly CallThreadCandidate[],
  roomJid: string | null | undefined,
  sid: string | null | undefined,
): string | null {
  const room = normalizeMucCallRoomJid(roomJid ?? "");
  const activeSid = sid?.trim();
  if (!room || !activeSid) return null;

  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (!message?.threadId || !message.callThread) continue;
    if (message.callThread.kind !== "muc") continue;
    if (message.callThread.sid !== activeSid) continue;
    return message.threadId;
  }

  return null;
}

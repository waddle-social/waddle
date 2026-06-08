import type { CallThreadAnchor } from "@/lib/chat-ui";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

type CallThreadCandidate = {
  threadId?: string;
  roomJid?: string;
  callThread?: CallThreadAnchor;
};

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
    if (message.roomJid && normalizeMucCallRoomJid(message.roomJid) !== room) continue;
    return message.threadId;
  }

  return null;
}

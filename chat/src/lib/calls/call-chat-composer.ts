import type { CallThreadAnchor } from "@/lib/chat-ui";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

type CallThreadCandidate = {
  threadId?: string;
  callThread?: CallThreadAnchor;
};

/**
 * The slice of `$callState` the in-call composer needs to locate its
 * call-chat thread. Kept structural so callers can pass the live store
 * value (or a test fixture) without importing the full discriminated union.
 */
type ActiveCallChat = {
  phase: string;
  kind?: "dm" | "muc";
  sid?: string | null;
};

/**
 * Resolve the active MUC call's XEP-0201 thread from the current
 * conversation's loaded timeline.
 *
 * A MUC anchor's `urn:waddle:call-thread:0` marker carries a throwaway
 * server-side session id that never equals the client's per-attempt call
 * sid, so we cannot match on sid. There is at most one active call per room,
 * so the thread is simply the most recent **non-ended** `muc` anchor in the
 * room-scoped timeline (`ContentArea` scopes `messages` to one conversation;
 * timeline messages do not carry a room JID themselves, so `roomJid` only
 * guards that a room is in context).
 *
 * Assumption: the most recent non-ended `muc` anchor is the active call. The
 * server emits exactly one anchor per call start and an XEP-0422 ended
 * fastening on call end, so this holds in steady state. A prior call whose
 * ended fastening was missed live (and not yet reconciled from MAM) could
 * momentarily mis-bind until the new call's anchor arrives; this is a strict
 * improvement over the previous sid match, which never resolved at all.
 */
export function resolveActiveMucCallThreadId(
  messages: readonly CallThreadCandidate[],
  roomJid: string | null | undefined,
): string | null {
  const room = normalizeMucCallRoomJid(roomJid ?? "");
  if (!room) return null;

  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (!message?.threadId || !message.callThread) continue;
    if (message.callThread.kind !== "muc") continue;
    if (message.callThread.ended) continue;
    return message.threadId;
  }

  return null;
}

/**
 * Resolve the call-chat thread id for the in-call composer of the currently
 * active call.
 *
 * - DM: the thread id equals the JMI session id by server invariant
 *   (`thread_id == key.sid.0`), which is exactly the client's active call
 *   sid — so it resolves live without waiting for a timeline anchor.
 * - MUC: resolved from the room timeline (see
 *   {@link resolveActiveMucCallThreadId}).
 */
export function activeCallChatThreadId(
  call: ActiveCallChat,
  messages: readonly CallThreadCandidate[],
  roomJid: string | null | undefined,
): string | null {
  if (call.phase !== "active") return null;
  if (call.kind === "dm") {
    const sid = call.sid?.trim();
    return sid ? sid : null;
  }
  if (call.kind === "muc") {
    return resolveActiveMucCallThreadId(messages, roomJid);
  }
  return null;
}

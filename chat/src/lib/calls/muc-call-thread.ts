import { map } from "nanostores";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

/**
 * Normalized room JID → the active MUC call's XEP-0201 thread id, captured from
 * the broadcast `urn:waddle:call-thread:0` anchor the moment it is received.
 *
 * The in-call composer reads from here so it can resolve the call-chat thread
 * without the anchor message being present in the currently-loaded channel
 * timeline (the anchor can be routed to notifications when it arrives before
 * the room is "current", and the timeline window may not contain it).
 */
export const $mucCallThreadId = map<Record<string, string>>({});

export function rememberMucCallThread(roomJid: string, threadId: string): void {
  const room = normalizeMucCallRoomJid(roomJid);
  const id = threadId.trim();
  if (!room || !id) return;
  if ($mucCallThreadId.get()[room] === id) return;
  $mucCallThreadId.setKey(room, id);
}

export function forgetMucCallThread(roomJid: string): void {
  const room = normalizeMucCallRoomJid(roomJid);
  if (!room) return;
  const current = $mucCallThreadId.get();
  if (!(room in current)) return;
  const next = { ...current };
  delete next[room];
  $mucCallThreadId.set(next);
}

export function readMucCallThread(
  roomJid: string,
  snapshot: Record<string, string> = $mucCallThreadId.get(),
): string | null {
  const room = normalizeMucCallRoomJid(roomJid);
  if (!room) return null;
  return snapshot[room] ?? null;
}

/** Drop all captured call-thread ids — on logout/disconnect, alongside the
 *  other per-room call stores, so a stale entry can't outlive the session. */
export function clearMucCallThreads(): void {
  if (Object.keys($mucCallThreadId.get()).length === 0) return;
  $mucCallThreadId.set({});
}

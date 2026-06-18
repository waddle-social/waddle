import { atom } from "nanostores";
import { $callState } from "./call-store";

/**
 * Inputs to the in-call Chat unread reducer: the ordered ids of inbound
 * (non-self) call-thread messages currently loaded, the last-seen watermark,
 * and whether the Chat tab is the focused dock tab right now.
 */
export type CallChatUnreadInput = {
  inboundIds: readonly string[];
  lastSeenId: string | null;
  focused: boolean;
};

export type CallChatUnreadResult = {
  unread: number;
  lastSeenId: string | null;
};

/**
 * The slice of a timeline message the unread selector needs. Kept structural
 * so callers can pass live `TimelineMessage`s or test fixtures alike.
 */
export type CallChatThreadMessage = {
  id: string;
  threadId?: string | null;
  isSelf?: boolean;
};

/**
 * Ordered ids of inbound (not-self) messages on the active call-chat thread.
 * Empty when no call thread is active.
 */
export function inboundCallChatThreadIds(
  messages: readonly CallChatThreadMessage[],
  threadId: string | null,
): string[] {
  if (!threadId) return [];
  const ids: string[] = [];
  for (const message of messages) {
    if (message.threadId !== threadId) continue;
    if (message.isSelf) continue;
    ids.push(message.id);
  }
  return ids;
}

/**
 * Pure reducer for the Chat tab's unread badge.
 *
 * While the Chat tab is unfocused, every inbound call-thread message that
 * arrived after the watermark counts as unread and the watermark holds still.
 */
export function nextCallChatUnread(input: CallChatUnreadInput): CallChatUnreadResult {
  const { inboundIds, lastSeenId, focused } = input;
  if (focused) {
    // Reading the tab marks everything currently loaded as seen.
    return { unread: 0, lastSeenId: inboundIds.at(-1) ?? lastSeenId };
  }
  const seenIndex = lastSeenId ? inboundIds.lastIndexOf(lastSeenId) : -1;
  const unread = inboundIds.length - (seenIndex + 1);
  return { unread, lastSeenId };
}

/**
 * The Chat tab's live unread count. A plain module-scoped atom, not persisted:
 * it tracks one call's session and starts fresh on the next.
 */
export const $callChatUnread = atom<number>(0);

// The read watermark behind the badge. Module-scoped (not a component ref) so
// it survives the split⟷expanded surface remounts within one call.
let lastSeenId: string | null = null;

/**
 * Fold the current inbound call-thread ids and Chat-tab focus into the badge.
 * Called whenever the loaded messages or the focus state change.
 */
export function syncCallChatUnread(
  inboundIds: readonly string[],
  focused: boolean,
): void {
  const result = nextCallChatUnread({ inboundIds, lastSeenId, focused });
  lastSeenId = result.lastSeenId;
  $callChatUnread.set(result.unread);
}

/** Clear the badge and the watermark — used at the start/end of a call. */
export function resetCallChatUnread(): void {
  lastSeenId = null;
  $callChatUnread.set(0);
}

// Reset whenever the call leaves the active phase so a stale unread count can't
// carry into the next call. Subscribed once at module load.
$callState.subscribe((state) => {
  if (state.phase !== "active") resetCallChatUnread();
});

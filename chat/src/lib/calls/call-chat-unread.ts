import { atom } from "nanostores";
import { $callState } from "./call-store";
import type { CallDockTab } from "./call-dock-state";

/**
 * The Chat tab is "focused" — and therefore reads incoming messages live —
 * only when the dock is open on the Chat tab. While the dock is closed or
 * parked on Participants, inbound messages accumulate as unread.
 */
export function isCallChatTabFocused(dockOpen: boolean, tab: CallDockTab): boolean {
  return dockOpen && tab === "chat";
}

/**
 * Inputs to the in-call Chat unread reducer: the ordered ids of inbound
 * (non-self) call-thread messages currently loaded, the last-seen watermark,
 * and whether the Chat tab is the focused dock tab right now.
 */
type CallChatUnreadInput = {
  inboundIds: readonly string[];
  lastSeenId: string | null;
  focused: boolean;
};

type CallChatUnreadResult = {
  unread: number;
  lastSeenId: string | null;
};

/**
 * The slice of a timeline message the unread selector needs. Kept structural
 * so callers can pass live `TimelineMessage`s or test fixtures alike.
 */
type CallChatThreadMessage = {
  id: string;
  threadId?: string | null;
  isSelf?: boolean;
  /** Present on the call-anchor system card (not a chat message). */
  callThread?: unknown;
};

/**
 * Whether a thread message is an actual call-chat message: on the active call
 * thread, not the local user's own, and not the bodyless call-anchor card
 * (which shares the thread id — DM anchors use `threadId === sid`, MUC anchors
 * are the thread root).
 */
function isInboundCallChatMessage(
  message: CallChatThreadMessage,
  threadId: string,
): boolean {
  return (
    message.threadId === threadId && !message.isSelf && !message.callThread
  );
}

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
    if (isInboundCallChatMessage(message, threadId)) ids.push(message.id);
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
  // If the watermark id is no longer in the loaded slice (-1), everything loaded
  // counts as unread. Safe for in-call chat: the thread is short-lived and never
  // paged/trimmed today. Revisit with a position/time sentinel if the Immersive
  // rework (#1028) starts paging this thread.
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
  // Skip a no-op write so subscribers aren't notified when the count is unchanged.
  if ($callChatUnread.get() !== result.unread) $callChatUnread.set(result.unread);
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

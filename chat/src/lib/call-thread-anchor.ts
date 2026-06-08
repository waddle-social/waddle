import { computed, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import { $callState } from "@/lib/calls/call-store";
import { isBusy } from "@/lib/calls/call-activity-dock";
import { $mucCallParticipants, normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { readRoomHasActiveCall, useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";

export interface CallAnchorCardState {
  status: "live" | "ended";
  media: CallMedia;
  participantCount: number;
  participantLabels: string[];
  messageCount: number;
  threadId: string | null;
  title: string;
  actionLabel: "Join" | "Rejoin" | "In another call" | null;
  actionDisabled: boolean;
  ariaLabel: string;
}

export function callThreadAnchorLabel(message: Pick<TimelineMessage, "body" | "author" | "callThread">): string {
  if (message.callThread?.ended && message.callThread.duration) {
    return `Call ended · ${formatCallThreadDuration(message.callThread.duration)}`;
  }
  return message.body || `${message.author} started a call`;
}

export function callThreadAnchorThreadId(message: Pick<TimelineMessage, "threadId" | "callThread">): string | null {
  return message.callThread && message.threadId ? message.threadId : null;
}

/**
 * Adapts a global Threads-query entry into the message shape the call-anchor
 * card state builder consumes, so a thread-list row can feed
 * `readCallAnchorCardState`/`useCallAnchorCardState` without a timeline message.
 *
 * Returns `null` for entries that don't anchor a MUC call (DM call anchors and
 * plain threads), matching the channel feed which only renders MUC anchors.
 */
export function wasmThreadEntryToAnchorMessage(
  entry: WasmThreadEntry,
): Pick<TimelineMessage, "body" | "author" | "callThread" | "threadId"> | null {
  if (!entry.callThread || entry.callThread.kind !== "muc") return null;
  return {
    body: "",
    author: "",
    threadId: entry.thread_id,
    callThread: {
      kind: "muc",
      media: entry.callThread.media,
      ...(entry.callThreadEnded
        ? { ended: entry.callThreadEnded.ended, duration: entry.callThreadEnded.duration }
        : {}),
    },
  };
}

export function readCallAnchorCardState(
  message: Pick<TimelineMessage, "body" | "author" | "callThread" | "threadId">,
  roomJid: string,
  messageCount = 0,
): CallAnchorCardState | null {
  if (!message.callThread) return null;
  const room = normalizeMucCallRoomJid(roomJid);
  const roomState = readRoomHasActiveCall(room);
  const callState = $callState.get();
  const participantLabels = room ? [...($mucCallParticipants.get()[room] ?? [])] : [];
  return buildCallAnchorCardState({
    message,
    roomJid: room,
    hasActiveCall: roomState.hasActiveCall,
    localResourceInCall: roomState.localResourceInCall,
    callState,
    media: roomState.media,
    participantCount: roomState.participantCount,
    participantLabels,
    messageCount,
  });
}

export function useCallAnchorCardState(
  message: () => Pick<TimelineMessage, "body" | "author" | "callThread" | "threadId">,
  roomJid: () => string | null | undefined,
  messageCount: () => number = () => 0,
): ComputedRef<CallAnchorCardState | null> {
  const participants = useStore($mucCallParticipants);
  const callState = useStore($callState);
  const roomCall = useRoomHasActiveCall(() => roomJid() ?? "");
  return computed(() => {
    const current = message();
    const room = normalizeMucCallRoomJid(roomJid() ?? "");
    const participantLabels = room ? [...(participants.value[room] ?? [])] : [];
    return buildCallAnchorCardState({
      message: current,
      roomJid: room,
      hasActiveCall: roomCall.hasActiveCall.value,
      localResourceInCall: roomCall.localResourceInCall.value,
      callState: callState.value,
      media: roomCall.media.value,
      participantCount: roomCall.participantCount.value,
      participantLabels,
      messageCount: messageCount(),
    });
  });
}

function buildCallAnchorCardState(options: {
  message: Pick<TimelineMessage, "body" | "author" | "callThread" | "threadId">;
  roomJid: string;
  hasActiveCall: boolean;
  localResourceInCall: boolean;
  callState: ReturnType<typeof $callState.get>;
  media: CallMedia;
  participantCount: number;
  participantLabels: string[];
  messageCount: number;
}): CallAnchorCardState | null {
  const callThread = options.message.callThread;
  if (!callThread) return null;
  const { message, media, participantCount, participantLabels, messageCount } = options;
  const live = !callThread.ended && options.hasActiveCall;
  const localGroupCallInThisRoom = isLocalGroupCallInRoom(options.callState, options.roomJid);
  const busy = live && isBusy(options.callState) && !localGroupCallInThisRoom;
  const retainedLocalResource = live && options.localResourceInCall && !localGroupCallInThisRoom;
  const joinAvailable = live && !busy && !localGroupCallInThisRoom;
  const mediaLabel = media.video ? "video call" : "call";
  const peopleLabel = `${participantCount} ${participantCount === 1 ? "person" : "people"}`;
  const participants = participantLabels.length > 0 ? `: ${participantLabels.join(", ")}` : "";

  return {
    status: live ? "live" : "ended",
    media,
    participantCount,
    participantLabels,
    messageCount,
    threadId: callThreadAnchorThreadId(message),
    title: live ? `Live ${mediaLabel}` : "Call ended",
    actionLabel: joinAvailable ? (retainedLocalResource ? "Rejoin" : "Join") : busy ? "In another call" : null,
    actionDisabled: busy,
    ariaLabel: live
      ? busy
        ? `Live ${mediaLabel}, ${peopleLabel}${participants}; already in another call`
        : retainedLocalResource
          ? `Rejoin live ${mediaLabel}, ${peopleLabel}${participants}`
          : `Join live ${mediaLabel}, ${peopleLabel}${participants}`
      : callThreadAnchorLabel(message),
  };
}

function isLocalGroupCallInRoom(
  state: ReturnType<typeof $callState.get>,
  roomJid: string,
): boolean {
  if (!roomJid) return false;
  if (state.phase !== "active" && state.phase !== "muc-pending") return false;
  if (state.kind !== "muc") return false;
  return normalizeMucCallRoomJid(state.peer) === roomJid;
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

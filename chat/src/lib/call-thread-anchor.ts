import { computed, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import { $callState } from "@/lib/calls/call-store";
import { isBusy } from "@/lib/calls/call-activity-dock";
import { $dmCallActivities } from "@/lib/calls/dm-call-activity";
import { $mucCallParticipants, normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { readRoomHasActiveCall, useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import { barePeerJid } from "@/lib/xmpp-client";

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
  const callState = $callState.get();
  if (message.callThread.kind === "dm") {
    const activity = readMatchingDmCallActivity(message, roomJid);
    return buildCallAnchorCardState({
      message,
      roomJid: "",
      hasActiveCall: !!activity,
      localResourceInCall: false,
      callState,
      media: activity?.media ?? { audio: false, video: false },
      participantCount: activity ? 2 : 0,
      participantLabels: activity ? [barePeerJid(roomJid)] : [],
      messageCount,
    });
  }
  const room = normalizeMucCallRoomJid(roomJid);
  const roomState = readRoomHasActiveCall(room);
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
  const dmActivities = useStore($dmCallActivities);
  const callState = useStore($callState);
  const roomCall = useRoomHasActiveCall(() => roomJid() ?? "");
  return computed(() => {
    const current = message();
    if (!current.callThread) return null;
    if (current.callThread.kind === "dm") {
      const peerJid = roomJid() ?? "";
      const activity = readMatchingDmCallActivity(current, peerJid, dmActivities.value);
      return buildCallAnchorCardState({
        message: current,
        roomJid: "",
        hasActiveCall: !!activity,
        localResourceInCall: false,
        callState: callState.value,
        media: activity?.media ?? { audio: false, video: false },
        participantCount: activity ? 2 : 0,
        participantLabels: activity ? [barePeerJid(peerJid)] : [],
        messageCount: messageCount(),
      });
    }
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

function readMatchingDmCallActivity(
  message: Pick<TimelineMessage, "callThread">,
  peerJid: string,
  activities = $dmCallActivities.get(),
) {
  const sid = message.callThread?.sid;
  if (!sid || message.callThread?.ended) return null;
  const peer = barePeerJid(peerJid).toLowerCase();
  return Object.values(activities).find((activity) =>
    barePeerJid(activity.peerJid).toLowerCase() === peer
    && activity.sid === sid
    && activity.state === "accepted"
  ) ?? null;
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
  const { message, participantCount, participantLabels, messageCount } = options;
  const live = !callThread.ended && options.hasActiveCall;
  // Live calls prefer the live detector (current media can differ from the
  // anchor's initial media). Ended calls must use the wire-authoritative anchor
  // media: the live detector is cleared when the call ends and falls back to an
  // audio-only default, which would mislabel an ended video call as audio.
  const media: CallMedia = live
    ? options.media
    : { audio: callThread.media.includes("audio"), video: callThread.media.includes("video") };
  // Action affordances are MUC-only: a group call has a room you can Join /
  // Rejoin and "In another call" busy semantics. A DM call is 1:1 — its
  // timeline card is a pure live/ended marker (answer/reconnect/return live in
  // the call dock and banner, never on the card). A DM "Join" would also be
  // malformed: the card's room JID is the peer JID, not a MUC room.
  const mucActions = callThread.kind === "muc";
  const localGroupCallInThisRoom = isLocalGroupCallInRoom(options.callState, options.roomJid);
  const busy = mucActions && live && isBusy(options.callState) && !localGroupCallInThisRoom;
  const retainedLocalResource = mucActions && live && options.localResourceInCall && !localGroupCallInThisRoom;
  const joinAvailable = mucActions && live && !busy && !localGroupCallInThisRoom;
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
    title: live
      ? `Live ${mediaLabel}`
      : callThread.ended && callThread.duration
        ? `Call ended · ${formatCallThreadDuration(callThread.duration)}`
        : "Call ended",
    actionLabel: joinAvailable ? (retainedLocalResource ? "Rejoin" : "Join") : busy ? "In another call" : null,
    actionDisabled: busy,
    // Keep the aria label in step with the rendered action: only announce
    // "Join"/"Rejoin" when that affordance actually exists (a joinable MUC
    // call). DM cards and a MUC call you are already in have no action, so
    // they announce a plain live marker.
    ariaLabel: live
      ? busy
        ? `Live ${mediaLabel}, ${peopleLabel}${participants}; already in another call`
        : joinAvailable
          ? retainedLocalResource
            ? `Rejoin live ${mediaLabel}, ${peopleLabel}${participants}`
            : `Join live ${mediaLabel}, ${peopleLabel}${participants}`
          : `Live ${mediaLabel}, ${peopleLabel}${participants}`
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

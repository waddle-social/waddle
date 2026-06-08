import { computed, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import { $mucCallParticipants, normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { readRoomHasActiveCall, useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";

export interface CallAnchorCardState {
  status: "live" | "ended";
  media: CallMedia;
  participantCount: number;
  participantLabels: string[];
  messageCount: number;
  threadId: string | null;
  title: string;
  actionLabel: "Join" | null;
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

export function readCallAnchorCardState(
  message: Pick<TimelineMessage, "body" | "author" | "callThread" | "threadId">,
  roomJid: string,
  messageCount = 0,
): CallAnchorCardState | null {
  if (!message.callThread) return null;
  const room = normalizeMucCallRoomJid(roomJid);
  const roomState = readRoomHasActiveCall(room);
  const participantLabels = room ? [...($mucCallParticipants.get()[room] ?? [])] : [];
  return buildCallAnchorCardState({
    message,
    hasActiveCall: roomState.hasActiveCall,
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
  const roomCall = useRoomHasActiveCall(() => roomJid() ?? "");
  return computed(() => {
    const current = message();
    const room = normalizeMucCallRoomJid(roomJid() ?? "");
    const participantLabels = room ? [...(participants.value[room] ?? [])] : [];
    return buildCallAnchorCardState({
      message: current,
      hasActiveCall: roomCall.hasActiveCall.value,
      media: roomCall.media.value,
      participantCount: roomCall.participantCount.value,
      participantLabels,
      messageCount: messageCount(),
    });
  });
}

function buildCallAnchorCardState(options: {
  message: Pick<TimelineMessage, "body" | "author" | "callThread" | "threadId">;
  hasActiveCall: boolean;
  media: CallMedia;
  participantCount: number;
  participantLabels: string[];
  messageCount: number;
}): CallAnchorCardState | null {
  const callThread = options.message.callThread;
  if (!callThread) return null;
  const { message, media, participantCount, participantLabels, messageCount } = options;
  const live = !callThread.ended && options.hasActiveCall;
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
    actionLabel: live ? "Join" : null,
    ariaLabel: live ? `Join live ${mediaLabel}, ${peopleLabel}${participants}` : callThreadAnchorLabel(message),
  };
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

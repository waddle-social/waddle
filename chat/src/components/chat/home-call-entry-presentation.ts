import {
  callActivityDockAction,
  canEndRecoveredDmCallActivity,
  type CallActivityDockEntry,
} from "@/lib/calls/call-activity-dock";
import type { CallState } from "@/lib/calls/types";
import { dmCallResumeBlockReason } from "@/lib/calls/dm-call-activity";
import { mucCallParticipantPreview } from "@/lib/calls/muc-call-indicators";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { readRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import { formatTimelineStamp } from "@/channels/timeline";

type ChannelCallEntry = Extract<CallActivityDockEntry, { kind: "channel" }>;

export type CallEntryVisualTone = "primary" | "success" | "warning";

export function callEntryStatus(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const noun = entry.participantCount === 1 ? "person" : "people";
    return `${entry.participantCount} ${noun}`;
  }
  if (entry.state === "accepted") return "Live";
  if (entry.direction === "outgoing") return "Calling";
  return "Ringing";
}

export function callEntryKindLabel(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const media = entry.media.video ? "Group video call" : "Group call";
    return entry.isKnownChannel ? media : `${media} syncing`;
  }
  if (entry.mediaKnown === false) return "Call";
  return entry.media.video ? "Video call" : "Voice call";
}

function callEntryAcceptedAvailability(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid: string | null,
): {
  eyebrow: string;
  meta: string;
  description: string;
  tone: CallEntryVisualTone;
} | null {
  if (entry.kind !== "dm" || entry.state !== "accepted") return null;
  const mediaLabel = entry.mediaKnown === false ? "call" : `${entry.media.video ? "video" : "voice"} call`;
  const mediaTitle = `${mediaLabel.charAt(0).toUpperCase()}${mediaLabel.slice(1)}`;
  const reason = dmCallResumeBlockReason(entry, selfFullJid);
  if (reason === null) {
    return {
      eyebrow: "Live now",
      meta: `Live ${mediaLabel}`,
      description: `The ${mediaLabel} is still live.`,
      tone: "success",
    };
  }
  if (reason === "other-resource") {
    return {
      eyebrow: "Other device",
      meta: `${mediaTitle} · Other device`,
      description: "This call is live on another browser or device.",
      tone: "warning",
    };
  }
  if (reason === "expired-token" || reason === "invalid-token") {
    if (canEndRecoveredDmCallActivity(entry, callState, selfFullJid)) {
      return {
        eyebrow: "Recovered after refresh",
        meta: `${mediaTitle} · End available`,
        description: `The saved reconnect details expired, but this tab can still end the call.`,
        tone: "warning",
      };
    }
    return {
      eyebrow: "Expired",
      meta: `${mediaTitle} · Expired`,
      description: "The saved reconnect details expired.",
      tone: "warning",
    };
  }
  return {
    eyebrow: "Syncing",
    meta: `${mediaTitle} · Details pending`,
    description: "Reconnect details are not available on this tab yet.",
    tone: "primary",
  };
}

export function callEntryVisualTone(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid: string | null,
): CallEntryVisualTone {
  if (entry.kind === "dm" && entry.state === "ringing" && entry.direction === "incoming") return "warning";
  if (entry.kind === "dm" && entry.state === "ringing" && entry.direction === "outgoing") return "primary";
  return callEntryAcceptedAvailability(entry, callState, selfFullJid)?.tone ?? "success";
}

export function callEntryEyebrow(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid: string | null,
): string {
  if (entry.kind === "channel") return entry.isActive ? "You're here" : "Live now";
  if (entry.isActive) return "You're here";
  const availability = callEntryAcceptedAvailability(entry, callState, selfFullJid);
  if (availability) return availability.eyebrow;
  if (entry.state === "accepted") return "Live now";
  if (entry.direction === "incoming") return "Incoming call";
  if (entry.direction === "outgoing") return "Calling";
  return "Ringing";
}

export function callEntryDescription(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid: string | null,
): string {
  if (entry.kind === "channel") {
    const noun = entry.participantCount === 1 ? "person" : "people";
    const location = entry.isKnownChannel ? "this channel" : "the channel";
    const preview = callEntryParticipantPreview(entry);
    if (entry.media.video) {
      if (preview) return `${entry.participantCount} ${noun} connected to the video call in ${location}: ${preview}.`;
      return `${entry.participantCount} ${noun} connected to the video call in ${location}.`;
    }
    if (preview) return `${entry.participantCount} ${noun} connected in ${location}: ${preview}.`;
    return `${entry.participantCount} ${noun} connected in ${location}.`;
  }

  const availability = callEntryAcceptedAvailability(entry, callState, selfFullJid);
  if (availability) return availability.description;

  if (entry.mediaKnown === false) {
    if (entry.state === "accepted") return "Call details are still syncing.";
    if (entry.direction === "incoming") return "Incoming call details are still syncing.";
    if (entry.direction === "outgoing") return "Outgoing call details are still syncing.";
    return "Call details are still syncing.";
  }

  const media = entry.media.video ? "video" : "voice";
  if (entry.state === "accepted") return `The ${media} call is still live.`;
  if (entry.direction === "incoming") return `Incoming ${media} call from this direct message.`;
  if (entry.direction === "outgoing") return `Outgoing ${media} call is still ringing.`;
  return `${media.charAt(0).toUpperCase()}${media.slice(1)} call is ringing.`;
}

export function callEntryDetail(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid: string | null,
): string {
  if (entry.kind === "channel") return callEntryKindLabel(entry);
  const stamp = entry.updatedAt ? formatTimelineStamp(entry.updatedAt) : "";
  return [
    callEntryAcceptedAvailability(entry, callState, selfFullJid)?.meta ?? callEntryKindLabel(entry),
    ...(stamp ? [`Updated ${stamp}`] : []),
  ].join(" · ");
}

export function callEntryParticipantPreview(entry: CallActivityDockEntry): string {
  if (entry.kind !== "channel") return "";
  return mucCallParticipantPreview(entry.participantLabels);
}

export function callEntryVisibleParticipantLabels(entry: CallActivityDockEntry): string[] {
  if (entry.kind !== "channel") return [];
  return entry.participantLabels.slice(0, 3);
}

export function callEntryParticipantInitial(label: string): string {
  return label.trim().charAt(0).toUpperCase() || "?";
}

export function callEntryToneClass(tone: CallEntryVisualTone): string {
  switch (tone) {
    case "warning":
      return "border-warning/25 bg-warning/10 hover:bg-warning/15";
    case "primary":
      return "border-primary/25 bg-primary/8 hover:bg-primary/12";
    case "success":
      return "border-success/20 bg-success/10 hover:bg-success/15";
  }
}

export function callEntryAccentClass(tone: CallEntryVisualTone): string {
  switch (tone) {
    case "warning":
      return "text-warning-foreground";
    case "primary":
      return "text-primary";
    case "success":
      return "text-success-foreground";
  }
}

export function callEntryIconClass(tone: CallEntryVisualTone): string {
  switch (tone) {
    case "warning":
      return "border-warning/25 bg-background/90 text-warning-foreground";
    case "primary":
      return "border-primary/25 bg-background/90 text-primary";
    case "success":
      return "border-success/25 bg-background/90 text-success-foreground";
  }
}

export function callEntryDotClass(tone: CallEntryVisualTone): string {
  switch (tone) {
    case "warning":
      return "bg-warning shadow-[0_0_6px_var(--warning)]";
    case "primary":
      return "bg-primary shadow-[0_0_6px_var(--primary)]";
    case "success":
      return "bg-success shadow-[0_0_6px_var(--success)]";
  }
}

export function callEntryPillClass(tone: CallEntryVisualTone): string {
  switch (tone) {
    case "warning":
      return "border-warning/25 bg-background/70 text-warning-foreground";
    case "primary":
      return "border-primary/25 bg-background/70 text-primary";
    case "success":
      return "border-success/25 bg-background/70 text-success-foreground";
  }
}

/**
 * Whether a retained channel entry can be left from Home: the local
 * resource is still in that room's call while the current tab call (if
 * any) points elsewhere.
 */
export function canLeaveRetainedChannelCallEntry(
  entry: ChannelCallEntry,
  callState: CallState,
): boolean {
  const roomJid = normalizeMucCallRoomJid(entry.roomJid);
  if (!roomJid || currentMucCallRoomJid(callState) === roomJid) return false;
  return readRoomHasActiveCall(roomJid).localResourceInCall;
}

function currentMucCallRoomJid(callState: CallState): string {
  if (callState.phase !== "active" && callState.phase !== "muc-pending") return "";
  if (callState.kind !== "muc") return "";
  return normalizeMucCallRoomJid(callState.peer);
}

export function isSameCallEntry(target: CallActivityDockEntry): (entry: CallActivityDockEntry) => boolean {
  return (entry) => {
    if (target.kind !== entry.kind) return false;
    if (target.kind === "channel" && entry.kind === "channel") {
      return normalizeMucCallRoomJid(target.roomJid) === normalizeMucCallRoomJid(entry.roomJid);
    }
    if (target.kind === "dm" && entry.kind === "dm") {
      return target.peerJid.toLowerCase() === entry.peerJid.toLowerCase() &&
        target.sid === entry.sid;
    }
    return false;
  };
}

export function callEntryActionLabel(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid: string | null,
): string {
  switch (callActivityDockAction(entry, callState, selfFullJid)) {
    case "answer":
      return "Answer";
    case "join":
      return entry.kind === "channel" && canLeaveRetainedChannelCallEntry(entry, callState) ? "Rejoin" : "Join";
    case "return":
      return "Return";
    case "reconnect":
      return "Reconnect";
    case "open":
      return "Open";
  }
}

function callEntryActionPhrase(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid: string | null,
): string {
  const action = callEntryActionLabel(entry, callState, selfFullJid);
  if (action === "Open") {
    return entry.kind === "dm"
      ? `Open ${entry.title} conversation`
      : `Open ${entry.title} channel`;
  }
  return `${action} ${entry.title}`;
}

/** Full accessible label for a Home active-call card. */
export function callEntryLabel(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid: string | null,
): string {
  return [
    callEntryActionPhrase(entry, callState, selfFullJid),
    callEntryKindLabel(entry),
    callEntryStatus(entry),
    callEntryEyebrow(entry, callState, selfFullJid),
    callEntryDescription(entry, callState, selfFullJid),
    callEntryDetail(entry, callState, selfFullJid),
  ].filter(Boolean).join(", ");
}

export function endCallEntryLabel(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") return `Leave ${entry.title} call`;
  const media = entry.mediaKnown !== false
    ? `${entry.media.video ? "video" : "voice"} call`
    : "call";
  return `End ${entry.title} ${media}`;
}

export function endCallEntryButtonText(entry: CallActivityDockEntry): string {
  return entry.kind === "channel" ? "Leave call" : "End call";
}

import type { ChannelSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp/types";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { CallMedia, CallState } from "./types";
import {
  callParticipantCountForChannel,
  callRoomJidForChannel,
  refreshedMucCallRooms,
} from "./muc-call-indicators";
import { normalizeMucCallRoomJid } from "./muc-call-presence";
import { canResumeDmCallActivity, hasKnownDmCallMedia, type DmCallActivity } from "./dm-call-activity";

export type SidebarMode = "channels" | "dms";

export type CallActivityDockEntry =
  | {
      kind: "channel";
      key: string;
      channelId: string | null;
      roomJid: string;
      title: string;
      participantCount: number;
      participantLabels: string[];
      isKnownChannel: boolean;
      isActive: boolean;
    }
  | {
      kind: "dm";
      key: string;
      peerJid: string;
      sid: string;
      remoteFullJid?: string;
      join?: DmCallActivity["join"];
      title: string;
      media: DmCallActivity["media"];
      mediaKnown?: boolean;
      state: DmCallActivity["state"];
      direction: DmCallActivity["direction"];
      updatedAt: string;
      isActive: boolean;
  };
type DmCallActivityDockEntry = Extract<CallActivityDockEntry, { kind: "dm" }>;

type CallActivityDockAction = "answer" | "open" | "join" | "return" | "reconnect";

type CallActivityDockSelection =
  | { kind: "channel"; channelId: string | null; roomJid: string }
  | { kind: "channel-join"; channelId: string | null; roomJid: string; media: CallMedia }
  | { kind: "dm-answer"; peerJid: string; remoteFullJid: string; sid: string; media: DmCallActivity["media"] }
  | { kind: "dm-open"; peerJid: string }
  | { kind: "dm-reconnect"; peerJid: string; media: DmCallActivity["media"] };

function activeMucCallRoomJid(state: CallState): string {
  if (state.phase !== "active" && state.phase !== "muc-pending") return "";
  if (state.kind !== "muc") return "";
  return normalizeMucCallRoomJid(state.peer);
}

function activeDmCallPeerJid(state: CallState): string {
  if (state.phase === "active" && state.kind === "dm") {
    return barePeerJid(state.peer).toLowerCase();
  }
  if (state.phase === "incoming") {
    return barePeerJid(state.from).toLowerCase();
  }
  if (state.phase === "outgoing") {
    return barePeerJid(state.to).toLowerCase();
  }
  return "";
}

export function isBusy(state: CallState): boolean {
  return state.phase !== "idle" && state.phase !== "ended";
}

export function dmCallActivityAction(
  activity: Pick<DmCallActivity, "peerJid" | "remoteFullJid" | "mediaKnown" | "state" | "direction" | "join">,
  callState: CallState,
  selfFullJid?: string | null,
): Extract<CallActivityDockAction, "answer" | "open" | "return" | "reconnect"> {
  const peerJid = barePeerJid(activity.peerJid).toLowerCase();
  if (peerJid && activeDmCallPeerJid(callState) === peerJid) return "return";
  if (
    activity.state === "ringing" &&
    activity.direction === "incoming" &&
    activity.remoteFullJid &&
    !isBusy(callState)
  ) return "answer";
  if (canResumeDmCallActivity(activity, selfFullJid) && !isBusy(callState)) return "reconnect";
  return "open";
}

export function callActivityDockAction(
  entry: CallActivityDockEntry,
  callState: CallState,
  selfFullJid?: string | null,
): CallActivityDockAction {
  if (entry.kind === "dm") {
    return dmCallActivityAction(entry, callState, selfFullJid);
  }

  const roomJid = normalizeMucCallRoomJid(entry.roomJid);
  if (!roomJid) return "open";
  if (activeMucCallRoomJid(callState) === roomJid) return "return";
  return isBusy(callState) ? "open" : "join";
}

export function callActivityDockSelection(
  entry: CallActivityDockEntry,
  callState: CallState = { phase: "idle" },
  selfFullJid?: string | null,
): CallActivityDockSelection {
  if (entry.kind === "channel") {
    if (callActivityDockAction(entry, callState, selfFullJid) === "join") {
      return {
        kind: "channel-join",
        channelId: entry.channelId,
        roomJid: entry.roomJid,
        media: { audio: true, video: false },
      };
    }
    return {
      kind: "channel",
      channelId: entry.channelId,
      roomJid: entry.roomJid,
    };
  }
  const action = callActivityDockAction(entry, callState, selfFullJid);
  if (action === "answer" && entry.remoteFullJid) {
    return {
      kind: "dm-answer",
      peerJid: entry.peerJid,
      remoteFullJid: entry.remoteFullJid,
      sid: entry.sid,
      media: entry.media,
    };
  }
  if (action === "reconnect") {
    return {
      kind: "dm-reconnect",
      peerJid: entry.peerJid,
      media: entry.media,
    };
  }
  return { kind: "dm-open", peerJid: entry.peerJid };
}

export function buildCallActivityDockEntries(options: {
  channels: ReadonlyArray<Pick<ChannelSummary, "id" | "name" | "jid">>;
  conversations: ReadonlyArray<Pick<DmConversation, "peerJid" | "peerUsername">>;
  activeChannelId: string | null;
  activeChannelRoomJid?: string | null;
  activePeerJid: string | null;
  sidebarMode: SidebarMode;
  activeChannelJids: Iterable<string>;
  managedMucDomain?: string | null;
  callParticipantCounts: Record<string, number>;
  callParticipants?: Record<string, readonly string[]>;
  dmCallActivities: Record<string, DmCallActivity>;
}): CallActivityDockEntry[] {
  const activeChannelRoomJid = normalizeMucCallRoomJid(options.activeChannelRoomJid ?? "");
  const participantLabelsByRoom = normalizedParticipantLabels(options.callParticipants ?? {});
  const channelEntries = options.channels.flatMap((channel): CallActivityDockEntry[] => {
    const participantCount = callParticipantCountForChannel(
      channel,
      options.callParticipantCounts,
      options.activeChannelJids,
      options.managedMucDomain,
    );
    if (participantCount <= 0) return [];
    const roomJid = callRoomJidForChannel(
      channel,
      options.callParticipantCounts,
      options.activeChannelJids,
      options.managedMucDomain,
    );
    return [{
      kind: "channel",
      key: `channel:${channel.id}`,
      channelId: channel.id,
      roomJid,
      title: channel.name,
      participantCount,
      participantLabels: participantLabelsByRoom.get(roomJid) ?? [],
      isKnownChannel: true,
      isActive: options.sidebarMode === "channels" && (
        options.activeChannelId === channel.id || activeChannelRoomJid === roomJid
      ),
    }];
  });

  const fallbackChannelEntries = refreshedMucCallRooms({
    channels: options.channels,
    activeChannelJids: options.activeChannelJids,
    managedMucDomain: options.managedMucDomain,
    callParticipantCounts: options.callParticipantCounts,
  })
    .map((room): CallActivityDockEntry => ({
      kind: "channel",
      key: `channel:${room.roomJid}`,
      channelId: null,
      roomJid: room.roomJid,
      title: room.title,
      participantCount: room.participantCount,
      participantLabels: participantLabelsByRoom.get(room.roomJid) ?? [],
      isKnownChannel: false,
      isActive: options.sidebarMode === "channels" && activeChannelRoomJid === room.roomJid,
    }));

  const conversationNames = new Map(
    options.conversations.map((conversation) => [
      barePeerJid(conversation.peerJid).toLowerCase(),
      conversation.peerUsername,
    ]),
  );
  const activePeer = barePeerJid(options.activePeerJid ?? "").toLowerCase();
  const dmEntries = Object.values(options.dmCallActivities)
    .map((activity): DmCallActivityDockEntry => {
      const peerJid = barePeerJid(activity.peerJid).toLowerCase();
      const fallbackName = peerJid.split("@")[0] || peerJid;
      return {
        kind: "dm",
        key: `dm:${peerJid}:${activity.sid}`,
        peerJid,
        sid: activity.sid,
        ...(activity.remoteFullJid ? { remoteFullJid: activity.remoteFullJid } : {}),
        ...(activity.join ? { join: activity.join } : {}),
        title: conversationNames.get(peerJid) ?? fallbackName,
        media: activity.media,
        ...(hasKnownDmCallMedia(activity) ? {} : { mediaKnown: false }),
        state: activity.state,
        direction: activity.direction,
        updatedAt: activity.updatedAt,
        isActive: options.sidebarMode === "dms" && activePeer === peerJid,
      };
    })
    .sort((left, right) => {
      const rightMs = Date.parse(right.updatedAt);
      const leftMs = Date.parse(left.updatedAt);
      if (Number.isFinite(rightMs) && Number.isFinite(leftMs) && rightMs !== leftMs) {
        return rightMs - leftMs;
      }
      return left.title.localeCompare(right.title);
    });

  return [...channelEntries, ...fallbackChannelEntries, ...dmEntries];
}

function normalizedParticipantLabels(
  participants: Record<string, readonly string[]>,
): Map<string, string[]> {
  const labels = new Map<string, string[]>();
  for (const [roomJid, nicks] of Object.entries(participants)) {
    const normalized = normalizeMucCallRoomJid(roomJid);
    if (!normalized) continue;
    const current = labels.get(normalized) ?? [];
    labels.set(normalized, [
      ...new Set([
        ...current,
        ...nicks.map((nick) => nick.trim()).filter(Boolean),
      ]),
    ]);
  }
  return labels;
}

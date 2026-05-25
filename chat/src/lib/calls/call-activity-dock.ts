import type { ChannelSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp/types";
import { barePeerJid } from "@/lib/xmpp/jid";
import {
  callParticipantCountForChannel,
  candidateRoomJidsForChannel,
} from "./muc-call-indicators";
import { normalizeMucCallRoomJid } from "./muc-call-presence";
import type { DmCallActivity } from "./dm-call-activity";

export type SidebarMode = "channels" | "dms";

export type CallActivityDockEntry =
  | {
      kind: "channel";
      key: string;
      channelId: string | null;
      roomJid: string;
      title: string;
      participantCount: number;
      isKnownChannel: boolean;
      isActive: boolean;
    }
  | {
      kind: "dm";
      key: string;
      peerJid: string;
      title: string;
      media: DmCallActivity["media"];
      state: DmCallActivity["state"];
      direction: DmCallActivity["direction"];
      updatedAt: string;
      isActive: boolean;
    };
type DmCallActivityDockEntry = Extract<CallActivityDockEntry, { kind: "dm" }>;

export function buildCallActivityDockEntries(options: {
  channels: ReadonlyArray<Pick<ChannelSummary, "id" | "name" | "jid">>;
  conversations: ReadonlyArray<Pick<DmConversation, "peerJid" | "peerUsername">>;
  activeChannelId: string | null;
  activeChannelRoomJid?: string | null;
  activePeerJid: string | null;
  sidebarMode: SidebarMode;
  activeChannelJids: Iterable<string>;
  callParticipantCounts: Record<string, number>;
  dmCallActivities: Record<string, DmCallActivity>;
}): CallActivityDockEntry[] {
  const matchedRoomJids = new Set<string>();
  const activeChannelRoomJid = normalizeMucCallRoomJid(options.activeChannelRoomJid ?? "");
  const channelEntries = options.channels.flatMap((channel): CallActivityDockEntry[] => {
    const participantCount = callParticipantCountForChannel(
      channel,
      options.callParticipantCounts,
      options.activeChannelJids,
    );
    if (participantCount <= 0) return [];
    const roomJid = candidateRoomJidsForChannel(channel, options.activeChannelJids)
      .map(normalizeMucCallRoomJid)
      .find((jid) => (options.callParticipantCounts[jid] ?? 0) > 0) ?? "";
    if (roomJid) matchedRoomJids.add(roomJid);
    return [{
      kind: "channel",
      key: `channel:${channel.id}`,
      channelId: channel.id,
      roomJid,
      title: channel.name,
      participantCount,
      isKnownChannel: true,
      isActive: options.sidebarMode === "channels" && (
        options.activeChannelId === channel.id || activeChannelRoomJid === roomJid
      ),
    }];
  });

  const fallbackChannelEntries = Object.entries(options.callParticipantCounts)
    .flatMap(([roomJid, participantCount]): CallActivityDockEntry[] => {
      const normalizedRoomJid = normalizeMucCallRoomJid(roomJid);
      if (!normalizedRoomJid || participantCount <= 0 || matchedRoomJids.has(normalizedRoomJid)) {
        return [];
      }
      const title = normalizedRoomJid.split("@")[0] ?? normalizedRoomJid;
      return [{
        kind: "channel",
        key: `channel:${normalizedRoomJid}`,
        channelId: null,
        roomJid: normalizedRoomJid,
        title,
        participantCount,
        isKnownChannel: false,
        isActive: options.sidebarMode === "channels" && activeChannelRoomJid === normalizedRoomJid,
      }];
    })
    .sort((left, right) => left.title.localeCompare(right.title));

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
        title: conversationNames.get(peerJid) ?? fallbackName,
        media: activity.media,
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

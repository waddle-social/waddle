import type { ChannelSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp/types";
import { barePeerJid } from "@/lib/xmpp/jid";
import {
  callParticipantCountForChannel,
} from "./muc-call-indicators";
import type { DmCallActivity } from "./dm-call-activity";

export type SidebarMode = "channels" | "dms";

export type CallActivityDockEntry =
  | {
      kind: "channel";
      key: string;
      channelId: string;
      title: string;
      participantCount: number;
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
  activePeerJid: string | null;
  sidebarMode: SidebarMode;
  activeChannelJids: Iterable<string>;
  callParticipantCounts: Record<string, number>;
  dmCallActivities: Record<string, DmCallActivity>;
}): CallActivityDockEntry[] {
  const channelEntries = options.channels.flatMap((channel): CallActivityDockEntry[] => {
    const participantCount = callParticipantCountForChannel(
      channel,
      options.callParticipantCounts,
      options.activeChannelJids,
    );
    if (participantCount <= 0) return [];
    return [{
      kind: "channel",
      key: `channel:${channel.id}`,
      channelId: channel.id,
      title: channel.name,
      participantCount,
      isActive: options.sidebarMode === "channels" && options.activeChannelId === channel.id,
    }];
  });

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

  return [...channelEntries, ...dmEntries];
}

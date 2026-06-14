import type { ChannelSummary, GroupDmSummary, SpaceSummary } from "@/lib/chat-types";
import type { RosterContact } from "@/lib/xmpp/types";
import type { DmConversation } from "@/lib/xmpp-client";
import type { ChannelUnreadMap } from "@/home/activity";
import type { DmCallActivity } from "@/lib/calls/dm-call-activity";
import type { CallMedia } from "@/lib/calls/types";

export interface HomeDashboardProps {
  spaces: SpaceSummary[];
  channels: ChannelSummary[];
  contacts: RosterContact[];
  isLoading: boolean;
  channelUnreadMap?: ChannelUnreadMap;
  activeChannelJids?: Set<string>;
  dmConversations?: DmConversation[];
  groupDms?: GroupDmSummary[];
  callParticipantCounts?: Record<string, number>;
  callParticipants?: Record<string, string[]>;
  callMediaByRoom?: Record<string, CallMedia>;
  dmCallActivities?: Record<string, DmCallActivity>;
  managedMucDomain?: string | null;
  selfFullJid?: string | null;
}

interface HomeDashboardSources extends Omit<HomeDashboardProps, "channelUnreadMap"> {
  channelUnreadMap: ChannelUnreadMap;
  mentionedRoomJids: Record<string, number>;
}

export function buildHomeDashboardProps(sources: HomeDashboardSources): HomeDashboardProps {
  return {
    spaces: sources.spaces,
    channels: sources.channels,
    contacts: sources.contacts,
    isLoading: sources.isLoading,
    channelUnreadMap: buildHomeChannelUnreadMap(
      sources.channels,
      sources.channelUnreadMap,
      sources.mentionedRoomJids,
    ),
    activeChannelJids: sources.activeChannelJids,
    dmConversations: sources.dmConversations,
    groupDms: sources.groupDms,
    callParticipantCounts: sources.callParticipantCounts,
    callParticipants: sources.callParticipants,
    callMediaByRoom: sources.callMediaByRoom,
    dmCallActivities: sources.dmCallActivities,
    managedMucDomain: sources.managedMucDomain,
    selfFullJid: sources.selfFullJid,
  };
}

export function buildHomeChannelUnreadMap(
  channels: readonly Pick<ChannelSummary, "id" | "jid">[],
  unreadMap: ChannelUnreadMap,
  mentionedRoomJids: Record<string, number>,
): ChannelUnreadMap {
  const map: ChannelUnreadMap = {};
  for (const channel of channels) {
    const current = unreadMap[channel.id] ?? { unread: 0, mentions: 0 };
    const mentions = channel.jid ? mentionedRoomJids[bareJid(channel.jid)] ?? 0 : 0;
    map[channel.id] = {
      ...current,
      mentions: Math.max(current.mentions, mentions),
    };
  }
  return map;
}

function bareJid(jid: string): string {
  return jid.split("/")[0] ?? jid;
}

import type { ChannelSummary } from "@/lib/chat-types";
import { barePeerJid, jidDomainOrEmpty, jidLocalpart } from "@/lib/xmpp/jid";
import { mucCallMediaForRoom, normalizeMucCallRoomJid } from "./muc-call-presence";
import type { CallMedia } from "./types";

export function normalizeMucServiceDomain(serviceJid?: string | null): string {
  const bare = barePeerJid(serviceJid ?? "").trim().toLowerCase();
  if (!bare) return "";
  return bare.includes("@") ? jidDomainOrEmpty(bare) : bare;
}

function candidateRoomJidsForChannel(
  channel: Pick<ChannelSummary, "id" | "jid">,
  activeChannelJids: Iterable<string>,
  managedMucDomain?: string | null,
): string[] {
  const candidates: string[] = [];
  const channelRoomJid = normalizeMucCallRoomJid(channel.jid ?? "");
  const channelRoomDomain = jidDomainOrEmpty(channelRoomJid);
  const trustedDomain = channelRoomDomain || normalizeMucServiceDomain(managedMucDomain);
  if (channelRoomJid) candidates.push(channelRoomJid);
  if (!channelRoomJid && trustedDomain) {
    candidates.push(`${channel.id.toLowerCase()}@${trustedDomain}`);
  }
  for (const jid of activeChannelJids) {
    const normalized = normalizeMucCallRoomJid(jid);
    if (!normalized) continue;
    const localpart = jidLocalpart(normalized);
    const domain = jidDomainOrEmpty(normalized);
    if (trustedDomain && domain === trustedDomain && localpart === channel.id.toLowerCase()) {
      candidates.push(normalized);
    }
  }
  return [...new Set(candidates)];
}

export function callParticipantCountForChannel(
  channel: Pick<ChannelSummary, "id" | "jid">,
  callParticipantCounts: Record<string, number> | undefined,
  activeChannelJids: Iterable<string>,
  managedMucDomain?: string | null,
): number {
  if (!callParticipantCounts) return 0;
  const countsByRoomJid = normalizedMucCallParticipantCounts(callParticipantCounts);
  for (const jid of candidateRoomJidsForChannel(channel, activeChannelJids, managedMucDomain)) {
    const count = countsByRoomJid.get(normalizeMucCallRoomJid(jid)) ?? 0;
    if (count > 0) return count;
  }
  return 0;
}

export function callRoomJidForChannel(
  channel: Pick<ChannelSummary, "id" | "jid">,
  callParticipantCounts: Record<string, number> | undefined,
  activeChannelJids: Iterable<string>,
  managedMucDomain?: string | null,
): string {
  if (!callParticipantCounts) return "";
  const countsByRoomJid = normalizedMucCallParticipantCounts(callParticipantCounts);
  return candidateRoomJidsForChannel(channel, activeChannelJids, managedMucDomain)
    .map(normalizeMucCallRoomJid)
    .find((jid) => (countsByRoomJid.get(jid) ?? 0) > 0) ?? "";
}

export type RefreshedMucCallRoom = {
  key: string;
  roomJid: string;
  title: string;
  participantCount: number;
  media: CallMedia;
};

export function refreshedMucCallRooms(options: {
  channels: ReadonlyArray<Pick<ChannelSummary, "id" | "jid">>;
  activeChannelJids: Iterable<string>;
  managedMucDomain?: string | null;
  callParticipantCounts: Record<string, number> | undefined;
  callMediaByRoom?: Record<string, CallMedia>;
}): RefreshedMucCallRoom[] {
  const trustedDomain = normalizeMucServiceDomain(options.managedMucDomain);
  if (!trustedDomain || !options.callParticipantCounts) return [];

  const countsByRoomJid = normalizedMucCallParticipantCounts(options.callParticipantCounts);

  const matchedRoomJids = new Set<string>();
  for (const channel of options.channels) {
    for (const candidate of candidateRoomJidsForChannel(
      channel,
      options.activeChannelJids,
      trustedDomain,
    )) {
      const normalized = normalizeMucCallRoomJid(candidate);
      if (normalized && (countsByRoomJid.get(normalized) ?? 0) > 0) {
        matchedRoomJids.add(normalized);
      }
    }
  }

  return [...countsByRoomJid.entries()]
    .flatMap(([roomJid, participantCount]): RefreshedMucCallRoom[] => {
      const roomDomain = jidDomainOrEmpty(roomJid);
      if (roomDomain !== trustedDomain || matchedRoomJids.has(roomJid)) return [];
      const title = jidLocalpart(roomJid) || roomJid;
      return [{
        key: `group-call:${roomJid}`,
        roomJid,
        title,
        participantCount,
        media: mucCallMediaForRoom(roomJid, options.callMediaByRoom),
      }];
    })
    .sort((left, right) => left.title.localeCompare(right.title));
}

function normalizedMucCallParticipantCounts(
  callParticipantCounts: Record<string, number>,
): Map<string, number> {
  const countsByRoomJid = new Map<string, number>();
  for (const [roomJid, count] of Object.entries(callParticipantCounts)) {
    const normalized = normalizeMucCallRoomJid(roomJid);
    if (!normalized || count <= 0) continue;
    countsByRoomJid.set(normalized, (countsByRoomJid.get(normalized) ?? 0) + count);
  }
  return countsByRoomJid;
}

export function mucCallParticipantPreview(nicks: readonly string[] | undefined, maxVisible = 2): string {
  const visible = (nicks ?? []).map((nick) => nick.trim()).filter(Boolean);
  if (visible.length === 0) return "";
  if (visible.length <= maxVisible) return visible.join(", ");
  return `${visible.slice(0, maxVisible).join(", ")} +${visible.length - maxVisible}`;
}

type CollapsedGroupActivitySummary = {
  unread: number;
  mentions: number;
  hasActivity: boolean;
  callCount: number;
};

type CollapsedGroupBadgeModel = {
  callCount: number;
  notification:
    | { kind: "mentions"; count: number }
    | { kind: "unread"; count: number }
    | { kind: "activity" }
    | null;
};

export function collapsedGroupBadgeModel(
  summary: CollapsedGroupActivitySummary,
): CollapsedGroupBadgeModel {
  return {
    callCount: summary.callCount > 0 ? summary.callCount : 0,
    notification:
      summary.mentions > 0
        ? { kind: "mentions", count: summary.mentions }
        : summary.unread > 0
          ? { kind: "unread", count: summary.unread }
          : summary.hasActivity
            ? { kind: "activity" }
            : null,
  };
}

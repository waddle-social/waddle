import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";

interface ChannelGroup {
  id: string;
  name: string;
  space: SpaceSummary | null;
  channels: ChannelSummary[];
}

export const STANDALONE_CHANNEL_GROUP_ID = "synthetic:standalone";
export const STANDALONE_CHANNEL_GROUP_NAME = "Standalone channels";

function sortChannels(items: ChannelSummary[]) {
  return [...items].sort(
    (a, b) =>
      (a.position ?? 0) - (b.position ?? 0) ||
      a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
  );
}

export function groupChannelsBySpace(
  spaces: SpaceSummary[],
  channels: ChannelSummary[],
): ChannelGroup[] {
  const bySpace = new Map(spaces.map((space) => [space.id, space]));
  const grouped = new Map<string, ChannelSummary[]>();
  const standalone: ChannelSummary[] = [];

  for (const channel of channels) {
    if (channel.spaceId && bySpace.has(channel.spaceId)) {
      const list = grouped.get(channel.spaceId) ?? [];
      list.push(channel);
      grouped.set(channel.spaceId, list);
    } else {
      standalone.push(channel);
    }
  }

  const groups: ChannelGroup[] = spaces
    .map((space) => ({
      id: `space:${space.id}`,
      name: space.name,
      space,
      channels: sortChannels(grouped.get(space.id) ?? []),
    }));

  if (standalone.length > 0) {
    groups.push({
      id: STANDALONE_CHANNEL_GROUP_ID,
      name: STANDALONE_CHANNEL_GROUP_NAME,
      space: null,
      channels: sortChannels(standalone),
    });
  }

  return groups;
}

export function firstChannelIdForSpace(
  channels: ChannelSummary[],
  spaceId: string,
): string | null {
  return sortChannels(channels.filter((channel) => channel.spaceId === spaceId))[0]?.id ?? null;
}

export function firstStandaloneChannelId(channels: ChannelSummary[]): string | null {
  return sortChannels(channels.filter((channel) => !channel.spaceId))[0]?.id ?? null;
}

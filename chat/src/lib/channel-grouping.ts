import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";

interface ChannelGroup {
  id: string;
  name: string;
  space: SpaceSummary | null;
  channels: ChannelSummary[];
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

  const sortChannels = (items: ChannelSummary[]) =>
    [...items].sort(
      (a, b) =>
        (a.position ?? 0) - (b.position ?? 0) ||
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    );

  const groups: ChannelGroup[] = spaces
    .map((space) => ({
      id: space.id,
      name: space.name,
      space,
      channels: sortChannels(grouped.get(space.id) ?? []),
    }))
    .filter((group) => group.channels.length > 0);

  if (standalone.length > 0) {
    groups.push({
      id: "standalone",
      name: "Channels",
      space: null,
      channels: sortChannels(standalone),
    });
  }

  return groups;
}

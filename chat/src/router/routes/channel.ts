import { threadAndPinnedSearch } from "../codecs";

export interface ChannelMatch {
  readonly id: "channel";
  readonly params: { channelId: string };
  readonly search: { thread: string[]; pinned: boolean };
}

interface ChannelMatchInput {
  params: { channelId: string };
  search?: { thread?: string[]; pinned?: boolean };
}

export const channelRoute = {
  id: "channel" as const,
  match(input: ChannelMatchInput): ChannelMatch {
    return {
      id: "channel",
      params: { channelId: input.params.channelId },
      search: {
        thread: input.search?.thread ?? [],
        pinned: input.search?.pinned ?? false,
      },
    };
  },
  href(input: ChannelMatchInput): string {
    const search = threadAndPinnedSearch.encode({
      thread: input.search?.thread ?? [],
      pinned: input.search?.pinned ?? false,
    });
    return `/r/${encodeURIComponent(input.params.channelId)}${search}`;
  },
  tryParse(pathname: string, searchString: string): ChannelMatch | null {
    const segments = pathname.split("/").filter(Boolean);
    if (segments.length !== 2) return null;
    if (segments[0] !== "r") return null;
    const channelId = decodeURIComponent(segments[1]!);
    return {
      id: "channel",
      params: { channelId },
      search: threadAndPinnedSearch.decode(searchString),
    };
  },
};

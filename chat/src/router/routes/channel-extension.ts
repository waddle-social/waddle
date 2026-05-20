import { threadAndPinnedSearch } from "../codecs";

export interface ChannelExtensionMatch {
  readonly id: "channelExtension";
  readonly params: {
    channelId: string;
    pluginId: string;
    routeId: string;
  };
  readonly search: { thread: string[]; pinned: boolean };
}

interface ChannelExtensionMatchInput {
  params: { channelId: string; pluginId: string; routeId: string };
  search?: { thread?: string[]; pinned?: boolean };
}

export const channelExtensionRoute = {
  id: "channelExtension" as const,
  match(input: ChannelExtensionMatchInput): ChannelExtensionMatch {
    return {
      id: "channelExtension",
      params: { ...input.params },
      search: {
        thread: input.search?.thread ?? [],
        pinned: input.search?.pinned ?? false,
      },
    };
  },
  href(input: ChannelExtensionMatchInput): string {
    const search = threadAndPinnedSearch.encode({
      thread: input.search?.thread ?? [],
      pinned: input.search?.pinned ?? false,
    });
    return (
      `/r/${encodeURIComponent(input.params.channelId)}` +
      `/x/${encodeURIComponent(input.params.pluginId)}` +
      `/${encodeURIComponent(input.params.routeId)}` +
      search
    );
  },
  tryParse(pathname: string, searchString: string): ChannelExtensionMatch | null {
    const segments = pathname.split("/").filter(Boolean);
    if (segments.length !== 5) return null;
    if (segments[0] !== "r" || segments[2] !== "x") return null;
    if (!segments[1] || !segments[3] || !segments[4]) return null;
    return {
      id: "channelExtension",
      params: {
        channelId: decodeURIComponent(segments[1]),
        pluginId: decodeURIComponent(segments[3]),
        routeId: decodeURIComponent(segments[4]),
      },
      search: threadAndPinnedSearch.decode(searchString),
    };
  },
};

import { threadAndPinnedSearch } from "../codecs";

export interface GroupDmRoomMatch {
  readonly id: "groupDmRoom";
  readonly params: { roomJid: string };
  readonly search: { thread: string[]; pinned: boolean };
}

interface GroupDmRoomMatchInput {
  params: { roomJid: string };
  search?: { thread?: string[]; pinned?: boolean };
}

export const groupDmRoomRoute = {
  id: "groupDmRoom" as const,
  match(input: GroupDmRoomMatchInput): GroupDmRoomMatch {
    return {
      id: "groupDmRoom",
      params: { roomJid: input.params.roomJid },
      search: { thread: input.search?.thread ?? [], pinned: input.search?.pinned ?? false },
    };
  },
  href(input: GroupDmRoomMatchInput): string {
    const search = threadAndPinnedSearch.encode({
      thread: input.search?.thread ?? [],
      pinned: input.search?.pinned ?? false,
    });
    return `/dm/room/${encodeURIComponent(input.params.roomJid)}${search}`;
  },
  tryParse(pathname: string, searchString: string): GroupDmRoomMatch | null {
    const segments = pathname.split("/").filter(Boolean);
    if (segments.length !== 3) return null;
    if (segments[0] !== "dm" || segments[1] !== "room") return null;
    if (!segments[2]) return null;
    return {
      id: "groupDmRoom",
      params: { roomJid: decodeURIComponent(segments[2]) },
      search: threadAndPinnedSearch.decode(searchString),
    };
  },
};

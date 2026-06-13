import { threadAndPinnedSearch } from "../codecs";

export interface DmMatch {
  readonly id: "dm";
  readonly params: { username: string };
  readonly search: { thread: string[]; pinned: boolean };
}

interface DmMatchInput {
  params: { username: string };
  search?: { thread?: string[]; pinned?: boolean };
}

export const dmRoute = {
  id: "dm" as const,
  match(input: DmMatchInput): DmMatch {
    return {
      id: "dm",
      params: { username: input.params.username },
      search: { thread: input.search?.thread ?? [], pinned: input.search?.pinned ?? false },
    };
  },
  href(input: DmMatchInput): string {
    const search = threadAndPinnedSearch.encode({
      thread: input.search?.thread ?? [],
      pinned: input.search?.pinned ?? false,
    });
    return `/dm/${encodeURIComponent(input.params.username)}${search}`;
  },
  tryParse(pathname: string, searchString: string): DmMatch | null {
    const segments = pathname.split("/").filter(Boolean);
    if (segments.length !== 2) return null;
    if (segments[0] !== "dm") return null;
    if (!segments[1]) return null;
    return {
      id: "dm",
      params: { username: decodeURIComponent(segments[1]) },
      search: threadAndPinnedSearch.decode(searchString),
    };
  },
};

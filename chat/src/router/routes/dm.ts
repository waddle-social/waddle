import { threadSearch } from "../codecs";

export interface DmMatch {
  readonly id: "dm";
  readonly params: { username: string };
  readonly search: { thread: string[] };
}

interface DmMatchInput {
  params: { username: string };
  search?: { thread?: string[] };
}

export const dmRoute = {
  id: "dm" as const,
  match(input: DmMatchInput): DmMatch {
    return {
      id: "dm",
      params: { username: input.params.username },
      search: { thread: input.search?.thread ?? [] },
    };
  },
  href(input: DmMatchInput): string {
    const search = threadSearch.encode({ thread: input.search?.thread ?? [] });
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
      search: threadSearch.decode(searchString),
    };
  },
};

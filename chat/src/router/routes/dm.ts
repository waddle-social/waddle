import { threadAndPinnedSearch } from "../codecs";

export interface DmMatch {
  readonly id: "dm";
  readonly params: { peerJid: string };
  readonly search: { thread: string[]; pinned: boolean; scope?: "occupant" };
}

interface DmMatchInput {
  params: { peerJid: string };
  search?: { thread?: string[]; pinned?: boolean; scope?: "occupant" };
}

export const dmRoute = {
  id: "dm" as const,
  match(input: DmMatchInput): DmMatch {
    return {
      id: "dm",
      params: { peerJid: input.params.peerJid },
      search: { thread: input.search?.thread ?? [], pinned: input.search?.pinned ?? false, ...(input.search?.scope ? { scope: input.search.scope } : {}) },
    };
  },
  href(input: DmMatchInput): string {
    let search = threadAndPinnedSearch.encode({
      thread: input.search?.thread ?? [],
      pinned: input.search?.pinned ?? false,
    });
    if (input.search?.scope === "occupant") search += `${search ? "&" : "?"}scope=occupant`;
    return `/dm/${encodeURIComponent(input.params.peerJid)}${search}`;
  },
  tryParse(pathname: string, searchString: string): DmMatch | null {
    const segments = pathname.split("/").filter(Boolean);
    if (segments.length !== 2) return null;
    if (segments[0] !== "dm") return null;
    if (!segments[1]) return null;
    let peerJid: string;
    try {
      peerJid = decodeURIComponent(segments[1]);
    } catch {
      return null;
    }
    if (!/^[^@/\s]+@[^@/\s]+(?:\/.+)?$/u.test(peerJid)) return null;
    const scope = new URLSearchParams(searchString).get("scope");
    if (scope !== null && scope !== "occupant") return null;
    // Resource-carrying routes must state occupant scope. Account URLs are
    // bare, so a cold start never guesses scope from incomplete discovery.
    if (peerJid.includes("/") !== (scope === "occupant")) return null;
    return {
      id: "dm",
      params: { peerJid },
      search: { ...threadAndPinnedSearch.decode(searchString), ...(scope === "occupant" ? { scope } : {}) },
    };
  },
};

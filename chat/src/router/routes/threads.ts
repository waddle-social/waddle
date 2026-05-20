export interface ThreadsMatch {
  readonly id: "threads";
}

export const threadsRoute = {
  id: "threads" as const,
  match(): ThreadsMatch {
    return { id: "threads" };
  },
  href(): string {
    return "/threads";
  },
  tryParse(pathname: string, _searchString: string): ThreadsMatch | null {
    return pathname === "/threads" ? { id: "threads" } : null;
  },
};

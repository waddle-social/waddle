export interface FeedMatch {
  readonly id: "feed";
}

export const feedRoute = {
  id: "feed" as const,
  match(): FeedMatch {
    return { id: "feed" };
  },
  href(): string {
    return "/feed";
  },
  tryParse(pathname: string, _searchString: string): FeedMatch | null {
    return pathname === "/feed" ? { id: "feed" } : null;
  },
};

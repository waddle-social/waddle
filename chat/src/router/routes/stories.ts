export interface StoriesMatch {
  readonly id: "stories";
}

export const storiesRoute = {
  id: "stories" as const,
  match(): StoriesMatch {
    return { id: "stories" };
  },
  href(): string {
    return "/stories";
  },
  tryParse(pathname: string, _searchString: string): StoriesMatch | null {
    return pathname === "/stories" ? { id: "stories" } : null;
  },
};

export interface HomeMatch {
  readonly id: "home";
}

export const homeRoute = {
  id: "home" as const,
  match(): HomeMatch {
    return { id: "home" };
  },
  href(): string {
    return "/";
  },
  tryParse(pathname: string, _searchString: string): HomeMatch | null {
    if (pathname === "/" || pathname === "") return { id: "home" };
    return null;
  },
};

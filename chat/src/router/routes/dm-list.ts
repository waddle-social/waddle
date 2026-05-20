export interface DmListMatch {
  readonly id: "dmList";
}

export const dmListRoute = {
  id: "dmList" as const,
  match(): DmListMatch {
    return { id: "dmList" };
  },
  href(): string {
    return "/dm";
  },
  tryParse(pathname: string, _searchString: string): DmListMatch | null {
    return pathname === "/dm" ? { id: "dmList" } : null;
  },
};

export interface EventsMatch {
  readonly id: "events";
}

export const eventsRoute = {
  id: "events" as const,
  match(): EventsMatch {
    return { id: "events" };
  },
  href(): string {
    return "/events";
  },
  tryParse(pathname: string, _searchString: string): EventsMatch | null {
    return pathname === "/events" ? { id: "events" } : null;
  },
};

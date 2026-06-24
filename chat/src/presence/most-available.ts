// Inbound render: collapse a contact's several resource presences into the
// single Show to render (one dot). Most-available wins. See
// docs/adr/010-presence-statuses.md.

export type RenderedShow = "available" | "away" | "xa" | "dnd" | "offline";

// Render priority, highest first. A deliberate Do Not Disturb wins over
// any other online state so an explicit "do not disturb me" is never
// masked by another resource (ADR-010). Offline is the implicit floor.
const AVAILABILITY_ORDER: RenderedShow[] = ["dnd", "available", "away", "xa"];

export function mostAvailableShow(shows: RenderedShow[]): RenderedShow {
  for (const show of AVAILABILITY_ORDER) {
    if (shows.includes(show)) return show;
  }
  return "offline";
}

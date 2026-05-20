import { staticRoute } from "../define";

export interface EventsMatch {
  readonly id: "events";
}

export const eventsRoute = staticRoute("events", "/events");

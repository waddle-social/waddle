import { staticRoute } from "../define";

export interface RoomsMatch {
  readonly id: "rooms";
}

export const roomsRoute = staticRoute("rooms", "/rooms");

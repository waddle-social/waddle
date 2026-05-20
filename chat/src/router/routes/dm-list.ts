import { staticRoute } from "../define";

export interface DmListMatch {
  readonly id: "dmList";
}

export const dmListRoute = staticRoute("dmList", "/dm");

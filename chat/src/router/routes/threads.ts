import { staticRoute } from "../define";

export interface ThreadsMatch {
  readonly id: "threads";
}

export const threadsRoute = staticRoute("threads", "/threads");

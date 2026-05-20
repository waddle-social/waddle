import { staticRoute } from "../define";

export interface FeedMatch {
  readonly id: "feed";
}

export const feedRoute = staticRoute("feed", "/feed");

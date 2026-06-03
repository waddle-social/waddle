import { staticRoute } from "../define";

export interface UnreadMatch {
  readonly id: "unread";
}

export const unreadRoute = staticRoute("unread", "/unread");

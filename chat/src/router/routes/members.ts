import { staticRoute } from "../define";

export interface MembersMatch {
  readonly id: "members";
}

export const membersRoute = staticRoute("members", "/members");

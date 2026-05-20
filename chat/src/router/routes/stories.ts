import { staticRoute } from "../define";

export interface StoriesMatch {
  readonly id: "stories";
}

export const storiesRoute = staticRoute("stories", "/stories");

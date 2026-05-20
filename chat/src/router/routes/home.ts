import { staticRoute } from "../define";

export interface HomeMatch {
  readonly id: "home";
}

export const homeRoute = staticRoute("home", "/");

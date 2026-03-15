import type { APIRoute } from "astro";

import { auth } from "../../../lib/auth";

export const ALL: APIRoute = async ({ request }) => {
  return auth.handler(request);
};

import type { APIRoute } from "astro";

import { getAuth } from "../../../lib/auth";

export const ALL: APIRoute = async ({ request }) => {
  const auth = await getAuth();
  return auth.handler(request);
};

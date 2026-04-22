import type { APIRoute } from "astro";

import { getOpenIdConfiguration } from "../../lib/openid-config";

export const GET: APIRoute = async ({ request }) => {
  return getOpenIdConfiguration(request);
};

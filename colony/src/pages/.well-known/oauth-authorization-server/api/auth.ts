import type { APIRoute } from "astro";

import { proxyOpenIdConfiguration } from "../../../../lib/openid-config";

export const GET: APIRoute = async ({ request }) => {
  return proxyOpenIdConfiguration(request);
};

import type { APIRoute } from "astro";

import { getAuth } from "../../../lib/auth";
import { proxyOpenIdConfiguration } from "../../../lib/openid-config";

export const ALL: APIRoute = async ({ request }) => {
  const url = new URL(request.url);
  if (url.pathname.endsWith("/.well-known/openid-configuration")) {
    return proxyOpenIdConfiguration(request, url.pathname);
  }

  const auth = await getAuth();
  return auth.handler(request);
};

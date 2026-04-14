import type { APIRoute } from "astro";

import { getAuth } from "../../../lib/auth";
import { proxyOpenIdConfiguration } from "../../../lib/openid-config";

export const ALL: APIRoute = async ({ request }) => {
  const url = new URL(request.url);
  if (url.pathname.endsWith("/.well-known/openid-configuration")) {
    return proxyOpenIdConfiguration(request, url.pathname);
  }
  if (
    url.pathname.endsWith("/oauth2/token") &&
    !request.headers.get("DPoP")?.trim()
  ) {
    return new Response(
      JSON.stringify({
        error: "invalid_dpop_proof",
        error_description: "DPoP header is required",
      }),
      {
        status: 400,
        headers: {
          "Content-Type": "application/json",
          "Cache-Control": "no-store",
          Pragma: "no-cache",
        },
      },
    );
  }

  const auth = await getAuth();
  return auth.handler(request);
};

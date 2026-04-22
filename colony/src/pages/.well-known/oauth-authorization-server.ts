import type { APIRoute } from "astro";

import { getOAuthAuthorizationServerMetadata } from "../../lib/openid-config";

export const GET: APIRoute = async ({ request }) => {
  return getOAuthAuthorizationServerMetadata(request);
};

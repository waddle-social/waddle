import type { APIRoute } from "astro";
import { env } from "cloudflare:workers";
import { handleGiphyProxyRequest } from "@/lib/giphy-proxy";

/**
 * Same-origin Giphy proxy. `GIPHY_API_KEY` is a Cloudflare secret and
 * stays server-side; the browser only ever sees `/api/giphy?...` and a
 * trimmed response body. See `@/lib/giphy-proxy` for the handler logic.
 */
export const GET: APIRoute = ({ url }) =>
  handleGiphyProxyRequest(env.GIPHY_API_KEY, url.searchParams, fetch);

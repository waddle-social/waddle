import type { APIRoute } from "astro";
import { env } from "cloudflare:workers";
import { createGiphyRateLimiter, handleGiphyProxyRequest } from "@/lib/giphy-proxy";

const RATE_LIMIT_PER_MINUTE = 30;

const allowRequest = createGiphyRateLimiter(RATE_LIMIT_PER_MINUTE, 60_000);

/**
 * Same-origin Giphy proxy. `GIPHY_API_KEY` is a Cloudflare secret and
 * stays server-side; the browser only ever sees `/api/giphy?...` and a
 * trimmed response body. Per-IP rate limited because the route is
 * unauthenticated. See `@/lib/giphy-proxy` for the handler logic.
 */
export const GET: APIRoute = ({ url, request }) => {
  const clientKey = request.headers.get("CF-Connecting-IP") ?? "unknown";
  return handleGiphyProxyRequest(env.GIPHY_API_KEY, url.searchParams, fetch, () =>
    allowRequest(clientKey, Date.now()),
  );
};

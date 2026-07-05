/**
 * Server-side Giphy proxy logic for `/api/giphy`.
 *
 * `GIPHY_API_KEY` is a secret: it must never appear in page HTML or in
 * browser-visible request URLs. The picker therefore calls the
 * same-origin `/api/giphy` route, and only this Worker-side handler
 * attaches the key to the upstream Giphy request.
 *
 * Kept as a pure function of `(apiKey, params, fetchImpl)` so tests can
 * drive it without the `cloudflare:workers` runtime.
 */

const GIPHY_API_ORIGIN = "https://api.giphy.com";
const DEFAULT_LIMIT = 24;
const MAX_LIMIT = 50;
const MAX_QUERY_LENGTH = 100;

interface GiphyProxyGif {
  id: string;
  title: string;
  images: {
    fixed_height_small: { url: string };
    original: { url: string };
  };
}

export async function handleGiphyProxyRequest(
  apiKey: string | undefined,
  params: URLSearchParams,
  fetchImpl: typeof fetch,
): Promise<Response> {
  if (!apiKey) {
    return jsonError("GIF search is not configured", 503);
  }

  const query = (params.get("q") ?? "").trim().slice(0, MAX_QUERY_LENGTH);
  const upstream = new URL(
    query ? "/v1/gifs/search" : "/v1/gifs/trending",
    GIPHY_API_ORIGIN,
  );
  upstream.searchParams.set("api_key", apiKey);
  upstream.searchParams.set("limit", String(clampLimit(params.get("limit"))));
  // Rating is pinned server-side; a client-supplied `rating` is ignored.
  upstream.searchParams.set("rating", "g");
  if (query) upstream.searchParams.set("q", query);

  let payload: unknown;
  try {
    const response = await fetchImpl(upstream.toString());
    if (!response.ok) return jsonError("GIF search is unavailable", 502);
    payload = await response.json();
  } catch {
    return jsonError("GIF search is unavailable", 502);
  }

  // Shared-cache the trimmed result: the route is same-origin and
  // unauthenticated (auth lives on the XMPP server), so a short CDN
  // TTL is what bounds how fast anonymous traffic can burn the Giphy
  // key's quota. Responses are identical for identical params —
  // nothing user-specific is cached.
  return Response.json(
    { data: trimGiphyPayload(payload) },
    { headers: { "Cache-Control": "public, max-age=300" } },
  );
}

function clampLimit(raw: string | null): number {
  const parsed = Number.parseInt(raw ?? "", 10);
  if (Number.isNaN(parsed)) return DEFAULT_LIMIT;
  return Math.min(Math.max(parsed, 1), MAX_LIMIT);
}

/**
 * Reduce Giphy's response to exactly the fields the picker renders.
 * Never echo the upstream body: it carries analytics pingback URLs and
 * whatever else Giphy adds over time.
 */
function trimGiphyPayload(payload: unknown): GiphyProxyGif[] {
  if (typeof payload !== "object" || payload === null) return [];
  const data = (payload as { data?: unknown }).data;
  if (!Array.isArray(data)) return [];
  const trimmed: GiphyProxyGif[] = [];
  for (const entry of data) {
    const gif = trimGiphyEntry(entry);
    if (gif) trimmed.push(gif);
  }
  return trimmed;
}

function trimGiphyEntry(entry: unknown): GiphyProxyGif | null {
  if (typeof entry !== "object" || entry === null) return null;
  const { id, title, images } = entry as {
    id?: unknown;
    title?: unknown;
    images?: { fixed_height_small?: { url?: unknown }; original?: { url?: unknown } };
  };
  const preview = images?.fixed_height_small?.url;
  const original = images?.original?.url;
  if (typeof id !== "string" || typeof preview !== "string" || typeof original !== "string") {
    return null;
  }
  return {
    id,
    title: typeof title === "string" ? title : "",
    images: {
      fixed_height_small: { url: preview },
      original: { url: original },
    },
  };
}

function jsonError(message: string, status: number): Response {
  return Response.json({ error: message }, { status });
}

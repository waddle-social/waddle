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

/**
 * Fixed-window rate limiter for the proxy route. The route is
 * unauthenticated (auth lives on the XMPP server), so this per-client
 * budget is what bounds how fast anonymous traffic can burn the Giphy
 * key's quota — the Cache-Control below only helps organic repeat
 * queries in the browser, not adversarial unique-`q` traffic.
 * In-memory per Worker isolate: best-effort, zero-dependency.
 */
export function createGiphyRateLimiter(
  maxPerWindow: number,
  windowMs: number,
  maxKeys = 10_000,
): (key: string, nowMs: number) => boolean {
  const windows = new Map<string, { start: number; count: number }>();
  return (key, nowMs) => {
    const entry = windows.get(key);
    if (!entry || nowMs - entry.start >= windowMs) {
      // Hard cap the table by evicting the oldest-inserted keys (Map
      // preserves insertion order): a flood of fresh keys inside one
      // window must cost O(1) per request and bounded memory, or the
      // limiter itself becomes the amplification vector. At cap, an
      // evicted key regains a fresh budget — bounded memory wins over
      // per-key strictness.
      windows.delete(key);
      while (windows.size >= maxKeys) {
        const oldest = windows.keys().next().value;
        if (oldest === undefined) break;
        windows.delete(oldest);
      }
      windows.set(key, { start: nowMs, count: 1 });
      return true;
    }
    entry.count += 1;
    return entry.count <= maxPerWindow;
  };
}

/**
 * Normalize a client IP into a rate-limit bucket key. IPv6 collapses
 * to its /64 prefix — a single routed /64 (standard residential/VPS
 * allocation) holds 2^64 addresses, so keying on the full address
 * would hand an attacker unlimited fresh budgets. IPv4 and opaque
 * values pass through unchanged.
 */
export function clientRateKey(ip: string): string {
  if (!ip.includes(":")) return ip;
  const [head, tail = ""] = ip.split("::", 2);
  const headGroups = head === "" ? [] : head.split(":");
  const tailGroups = tail === "" ? [] : tail.split(":");
  const missing = 8 - headGroups.length - tailGroups.length;
  const groups = ip.includes("::")
    ? [...headGroups, ...Array<string>(Math.max(missing, 0)).fill("0"), ...tailGroups]
    : headGroups;
  return groups.slice(0, 4).join(":");
}

export async function handleGiphyProxyRequest(
  apiKey: string | undefined,
  params: URLSearchParams,
  fetchImpl: typeof fetch,
  rateGate?: () => boolean,
): Promise<Response> {
  if (!apiKey) {
    return jsonError("GIF search is not configured", 503);
  }
  if (rateGate && !rateGate()) {
    return Response.json(
      { error: "Too many GIF searches — try again shortly" },
      { status: 429, headers: { "Retry-After": "60" } },
    );
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

  // Response is a pure function of q+limit and nothing user-specific,
  // so browsers may cache repeat queries briefly. (Cloudflare does not
  // edge-cache dynamic responses on this header alone — the quota
  // bound comes from the rate limiter above, not from caching.)
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

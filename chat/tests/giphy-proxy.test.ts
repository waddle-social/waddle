import { describe, expect, test } from "bun:test";
import { clientRateKey, createGiphyRateLimiter, handleGiphyProxyRequest } from "../src/lib/giphy-proxy";

const KEY = "super-secret-giphy-key";

function giphyUpstreamBody() {
  return {
    data: [
      {
        id: "gif-1",
        title: "dancing penguin",
        slug: "dancing-penguin-gif-1",
        url: "https://giphy.com/gifs/dancing-penguin-gif-1",
        username: "someuser",
        analytics: { onload: { url: "https://giphy-analytics.example/pingback" } },
        images: {
          fixed_height_small: { url: "https://media.giphy.com/gif-1/100.gif", width: "178", height: "100" },
          original: { url: "https://media.giphy.com/gif-1/full.gif", width: "480", height: "270" },
          downsized_large: { url: "https://media.giphy.com/gif-1/large.gif" },
        },
      },
      {
        id: "gif-2",
        title: "",
        images: {
          fixed_height_small: { url: "https://media.giphy.com/gif-2/100.gif" },
          original: { url: "https://media.giphy.com/gif-2/full.gif" },
        },
      },
    ],
    meta: { status: 200, msg: "OK", response_id: "resp-123" },
    pagination: { total_count: 9999, count: 2, offset: 0 },
  };
}

function stubFetch(
  handler: (url: string) => Response | Promise<Response>,
): { calls: string[]; fetchImpl: typeof fetch } {
  const calls: string[] = [];
  const fetchImpl = (async (input: string | URL | Request) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    calls.push(url);
    return handler(url);
  }) as typeof fetch;
  return { calls, fetchImpl };
}

function okUpstream() {
  return stubFetch(() => Response.json(giphyUpstreamBody()));
}

describe("handleGiphyProxyRequest", () => {
  test("search: forwards q with the key upstream and returns a trimmed body without the key", async () => {
    const { calls, fetchImpl } = okUpstream();
    const res = await handleGiphyProxyRequest(KEY, new URLSearchParams({ q: "penguin" }), fetchImpl);

    expect(res.status).toBe(200);
    expect(calls).toHaveLength(1);
    const upstream = new URL(calls[0]!);
    expect(upstream.origin).toBe("https://api.giphy.com");
    expect(upstream.pathname).toBe("/v1/gifs/search");
    expect(upstream.searchParams.get("api_key")).toBe(KEY);
    expect(upstream.searchParams.get("q")).toBe("penguin");

    const bodyText = await res.text();
    expect(bodyText).not.toContain(KEY);
    const body = JSON.parse(bodyText) as { data: unknown[] };
    expect(body).toEqual({
      data: [
        {
          id: "gif-1",
          title: "dancing penguin",
          images: {
            fixed_height_small: { url: "https://media.giphy.com/gif-1/100.gif" },
            original: { url: "https://media.giphy.com/gif-1/full.gif" },
          },
        },
        {
          id: "gif-2",
          title: "",
          images: {
            fixed_height_small: { url: "https://media.giphy.com/gif-2/100.gif" },
            original: { url: "https://media.giphy.com/gif-2/full.gif" },
          },
        },
      ],
    });
  });

  test("no q hits trending", async () => {
    const { calls, fetchImpl } = okUpstream();
    const res = await handleGiphyProxyRequest(KEY, new URLSearchParams(), fetchImpl);
    expect(res.status).toBe(200);
    expect(new URL(calls[0]!).pathname).toBe("/v1/gifs/trending");
    expect(new URL(calls[0]!).searchParams.get("q")).toBeNull();
  });

  test("clamps limit, pins rating to g, and caps q length", async () => {
    const { calls, fetchImpl } = okUpstream();
    const longQuery = "p".repeat(500);
    await handleGiphyProxyRequest(
      KEY,
      new URLSearchParams({ q: longQuery, limit: "500", rating: "r" }),
      fetchImpl,
    );
    const upstream = new URL(calls[0]!);
    expect(upstream.searchParams.get("limit")).toBe("50");
    expect(upstream.searchParams.get("rating")).toBe("g");
    expect(upstream.searchParams.get("q")).toBe("p".repeat(100));
  });

  test("invalid limit falls back to the default of 24", async () => {
    const { calls, fetchImpl } = okUpstream();
    await handleGiphyProxyRequest(KEY, new URLSearchParams({ limit: "banana" }), fetchImpl);
    expect(new URL(calls[0]!).searchParams.get("limit")).toBe("24");
  });

  test("upstream non-OK response maps to 502 without leaking the key", async () => {
    const { fetchImpl } = stubFetch(() => new Response("giphy exploded", { status: 500 }));
    const res = await handleGiphyProxyRequest(KEY, new URLSearchParams({ q: "x" }), fetchImpl);
    expect(res.status).toBe(502);
    expect(await res.text()).not.toContain(KEY);
  });

  test("upstream fetch rejection maps to 502", async () => {
    const { fetchImpl } = stubFetch(() => {
      throw new Error(`connect failed for api_key=${KEY}`);
    });
    const res = await handleGiphyProxyRequest(KEY, new URLSearchParams({ q: "x" }), fetchImpl);
    expect(res.status).toBe(502);
    expect(await res.text()).not.toContain(KEY);
  });

  test("unparseable upstream body maps to 502", async () => {
    const { fetchImpl } = stubFetch(() => new Response("<html>not json</html>", { status: 200 }));
    const res = await handleGiphyProxyRequest(KEY, new URLSearchParams({ q: "x" }), fetchImpl);
    expect(res.status).toBe(502);
  });

  test("missing key maps to 503 without contacting Giphy", async () => {
    const { calls, fetchImpl } = okUpstream();
    for (const key of [undefined, ""]) {
      const res = await handleGiphyProxyRequest(key, new URLSearchParams({ q: "x" }), fetchImpl);
      expect(res.status).toBe(503);
    }
    expect(calls).toHaveLength(0);
  });

  test("malformed upstream entries are dropped from the trimmed body", async () => {
    const { fetchImpl } = stubFetch(() =>
      Response.json({
        data: [
          "not-an-object",
          { id: "no-images" },
          { id: 42, images: { fixed_height_small: { url: "x" }, original: { url: "y" } } },
          {
            id: "good",
            title: "ok",
            images: {
              fixed_height_small: { url: "https://media.giphy.com/good/100.gif" },
              original: { url: "https://media.giphy.com/good/full.gif" },
            },
          },
        ],
      }),
    );
    const res = await handleGiphyProxyRequest(KEY, new URLSearchParams({ q: "x" }), fetchImpl);
    const body = (await res.json()) as { data: Array<{ id: string }> };
    expect(body.data).toHaveLength(1);
    expect(body.data[0]!.id).toBe("good");
  });
});

describe("response caching", () => {
  test("successful responses carry a shared-cache Cache-Control so anonymous hits cannot burn Giphy quota unbounded", async () => {
    const { fetchImpl } = stubFetch(() => Response.json(giphyUpstreamBody()));
    const trending = await handleGiphyProxyRequest(KEY, new URLSearchParams(), fetchImpl);
    expect(trending.headers.get("Cache-Control")).toBe("public, max-age=300");

    const search = await handleGiphyProxyRequest(
      KEY,
      new URLSearchParams({ q: "cats" }),
      fetchImpl,
    );
    expect(search.headers.get("Cache-Control")).toBe("public, max-age=300");
  });

  test("error responses are not cacheable", async () => {
    const unconfigured = await handleGiphyProxyRequest(undefined, new URLSearchParams(), fetch);
    expect(unconfigured.headers.get("Cache-Control")).toBeNull();
  });
});

describe("rate limiting", () => {
  test("createGiphyRateLimiter allows up to the per-window budget per key, then blocks, then resets after the window", () => {
    const allow = createGiphyRateLimiter(3, 60_000);
    expect(allow("ip-a", 0)).toBe(true);
    expect(allow("ip-a", 1_000)).toBe(true);
    expect(allow("ip-a", 2_000)).toBe(true);
    expect(allow("ip-a", 3_000)).toBe(false);
    // Independent budgets per key.
    expect(allow("ip-b", 3_000)).toBe(true);
    // Window rollover restores the budget.
    expect(allow("ip-a", 60_001)).toBe(true);
  });

  test("a denied gate returns 429 without contacting Giphy and without a cache header", async () => {
    const { calls, fetchImpl } = stubFetch(() => Response.json(giphyUpstreamBody()));
    const response = await handleGiphyProxyRequest(
      KEY,
      new URLSearchParams({ q: "cats" }),
      fetchImpl,
      () => false,
    );
    expect(response.status).toBe(429);
    expect(calls.length).toBe(0);
    expect(response.headers.get("Cache-Control")).toBeNull();
    const body = (await response.json()) as { error: string };
    expect(body.error).toBe("Too many GIF searches — try again shortly");
  });

  test("an allowing gate leaves the happy path untouched", async () => {
    const { fetchImpl } = stubFetch(() => Response.json(giphyUpstreamBody()));
    const response = await handleGiphyProxyRequest(
      KEY,
      new URLSearchParams({ q: "cats" }),
      fetchImpl,
      () => true,
    );
    expect(response.status).toBe(200);
  });
});

describe("rate limiter hardening", () => {
  test("the key table is hard-capped: oldest keys are evicted instead of scanning/growing under a fresh-key flood", () => {
    const allow = createGiphyRateLimiter(1, 60_000, 2);
    expect(allow("k1", 0)).toBe(true);
    expect(allow("k1", 1)).toBe(false);
    // Two more unique keys within the same window evict k1 (cap = 2).
    expect(allow("k2", 2)).toBe(true);
    expect(allow("k3", 3)).toBe(true);
    // k1 was evicted, so it gets a fresh budget — bounded memory is
    // the guarantee; per-key strictness degrades gracefully at cap.
    expect(allow("k1", 4)).toBe(true);
    // k3 is still tracked (not evicted by k1's re-insert beyond cap...
    // k1's re-insert evicts the now-oldest k2).
    expect(allow("k3", 5)).toBe(false);
  });

  test("rate keys collapse IPv6 to the /64 prefix so one routed /64 cannot mint unlimited budgets", () => {
    expect(clientRateKey("2001:db8:1:2:3:4:5:6")).toBe("2001:db8:1:2");
    expect(clientRateKey("2001:db8:1:2:aaaa:bbbb:cccc:dddd")).toBe("2001:db8:1:2");
    // Compressed forms expand before truncation.
    expect(clientRateKey("2001:db8::5")).toBe("2001:db8:0:0");
    expect(clientRateKey("::1")).toBe("0:0:0:0");
    // IPv4 stays as-is.
    expect(clientRateKey("203.0.113.9")).toBe("203.0.113.9");
    expect(clientRateKey("unknown")).toBe("unknown");
  });

  test("the 429 advises a retry window", async () => {
    const { fetchImpl } = stubFetch(() => Response.json(giphyUpstreamBody()));
    const response = await handleGiphyProxyRequest(
      KEY,
      new URLSearchParams(),
      fetchImpl,
      () => false,
    );
    expect(response.status).toBe(429);
    expect(response.headers.get("Retry-After")).toBe("60");
  });
});

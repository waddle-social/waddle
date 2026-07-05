import { describe, expect, test } from "bun:test";
import { resolveGifPickerResponse } from "../src/lib/gif-picker-fetch";

function response(status: number, body?: unknown): Pick<Response, "ok" | "status" | "json"> {
  return {
    status,
    ok: status >= 200 && status < 300,
    json: () => Promise.resolve(body),
  };
}

const gif = {
  id: "g1",
  title: "penguin",
  images: {
    fixed_height_small: { url: "https://media.giphy.com/g1/100.gif" },
    original: { url: "https://media.giphy.com/g1/full.gif" },
  },
};

describe("resolveGifPickerResponse", () => {
  test("503 means not configured: results cleared, no error message", async () => {
    const state = await resolveGifPickerResponse(response(503, { error: "GIF search is not configured" }));
    expect(state).toEqual({ notConfigured: true, results: [], errorMessage: null });
  });

  test("429 clears results and reports a rate-limit message — NOT the not-configured panel", async () => {
    const state = await resolveGifPickerResponse(response(429, { error: "Too many GIF searches — try again shortly" }));
    expect(state.notConfigured).toBe(false);
    expect(state.results).toEqual([]);
    expect(state.errorMessage).toBe("Too many GIF searches — try again shortly.");
  });

  test("502 clears results and reports an unavailable message — NOT the not-configured panel", async () => {
    const state = await resolveGifPickerResponse(response(502, { error: "GIF search is unavailable" }));
    expect(state.notConfigured).toBe(false);
    expect(state.results).toEqual([]);
    expect(state.errorMessage).toBe("GIF search is unavailable right now.");
  });

  test("a success after a 503 fully recovers: results set, flags cleared", async () => {
    const failed = await resolveGifPickerResponse(response(503));
    expect(failed.notConfigured).toBe(true);
    const state = await resolveGifPickerResponse(response(200, { data: [gif] }));
    expect(state).toEqual({ notConfigured: false, results: [gif], errorMessage: null });
  });

  test("a success payload without data yields empty results, not a crash", async () => {
    const state = await resolveGifPickerResponse(response(200, {}));
    expect(state).toEqual({ notConfigured: false, results: [], errorMessage: null });
  });

  test("an unparseable success body degrades to the unavailable message", async () => {
    const state = await resolveGifPickerResponse({
      status: 200,
      ok: true,
      json: () => Promise.reject(new Error("bad json")),
    });
    expect(state.notConfigured).toBe(false);
    expect(state.results).toEqual([]);
    expect(state.errorMessage).toBe("GIF search is unavailable right now.");
  });
});

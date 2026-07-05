/**
 * Classify a `/api/giphy` proxy response into GifPicker view state.
 *
 * The proxy legitimately answers 503 (no key configured), 429 (per-IP
 * rate limit), and 502 (upstream failure) besides 200 — only the MOST
 * RECENT response may drive the "not configured" panel, and a failed
 * fetch must never leave a previous query's results on screen.
 * Kept as a pure function of the response so the branch matrix is
 * testable without the SSR harness.
 */

export interface GiphyGif {
  id: string;
  title: string;
  images: {
    fixed_height_small: { url: string };
    original: { url: string };
  };
}

export interface GifPickerFetchState {
  notConfigured: boolean;
  results: GiphyGif[];
  errorMessage: string | null;
}

const RATE_LIMITED_MESSAGE = "Too many GIF searches — try again shortly.";
export const GIF_SEARCH_UNAVAILABLE_MESSAGE = "GIF search is unavailable right now.";

export async function resolveGifPickerResponse(
  res: Pick<Response, "ok" | "status" | "json">,
): Promise<GifPickerFetchState> {
  if (res.status === 503) {
    return { notConfigured: true, results: [], errorMessage: null };
  }
  if (!res.ok) {
    return {
      notConfigured: false,
      results: [],
      errorMessage: res.status === 429 ? RATE_LIMITED_MESSAGE : GIF_SEARCH_UNAVAILABLE_MESSAGE,
    };
  }
  try {
    const payload = (await res.json()) as { data?: GiphyGif[] } | null;
    return { notConfigured: false, results: payload?.data ?? [], errorMessage: null };
  } catch {
    return { notConfigured: false, results: [], errorMessage: GIF_SEARCH_UNAVAILABLE_MESSAGE };
  }
}

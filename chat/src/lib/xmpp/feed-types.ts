/**
 * XEP-0472 Social Feed entry shapes. The wasm bridge serialises
 * `JsFeedEntry` from Rust; this file declares the parallel TS types
 * (the snake_case-to-camelCase boundary) and the codec helper.
 */

export interface FeedEntry {
  /** Pubsub item id assigned by the publisher (UUID). */
  id: string;
  title?: string;
  body: string;
  /** Author bare JID. */
  author?: string;
  /** Epoch ms; absent when the publisher omitted `<published/>`. */
  publishedMs?: number;
  link?: string;
}

export interface FeedPostInput {
  body: string;
  title?: string;
  author?: string;
  link?: string;
}

/** Snake-case shape emitted by the wasm bridge. */
export interface WasmFeedEntry {
  id: string;
  title?: string | null;
  body: string;
  author?: string | null;
  /** RFC3339 string or null. */
  published?: string | null;
  link?: string | null;
}

export function feedEntryFromWasm(entry: WasmFeedEntry): FeedEntry {
  const publishedMs = entry.published ? Date.parse(entry.published) : undefined;
  return {
    id: entry.id,
    body: entry.body,
    ...(entry.title ? { title: entry.title } : {}),
    ...(entry.author ? { author: entry.author } : {}),
    ...(typeof publishedMs === "number" && Number.isFinite(publishedMs) ? { publishedMs } : {}),
    ...(entry.link ? { link: entry.link } : {}),
  };
}

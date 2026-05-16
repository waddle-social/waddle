/**
 * XEP-0501 Story shapes mirrored from the wasm bridge.
 *
 * Stories are ephemeral pubsub items with an `expires` timestamp;
 * the chat filters expired entries locally so the UI fades them out
 * as soon as the countdown hits zero, without waiting for a server
 * roundtrip.
 */

export interface Story {
  /** Pubsub item id assigned by the publisher (UUID). */
  id: string;
  body?: string;
  /** Image / video URL. */
  mediaUrl?: string;
  /** Author bare JID. */
  author?: string;
  /** Epoch ms when the story was posted. */
  postedMs?: number;
  /** Epoch ms when the story expires; absent ⇒ never expires. */
  expiresMs?: number;
}

export interface StoryPostInput {
  body?: string;
  mediaUrl?: string;
  author?: string;
  /** Hours from now until expiry. Defaults to 24 server-side. */
  expiryHours?: number;
}

/** Snake-case shape emitted by the wasm bridge. */
export interface WasmStory {
  id: string;
  body?: string | null;
  media_url?: string | null;
  author?: string | null;
  /** RFC3339 string or null. */
  posted?: string | null;
  /** RFC3339 string or null. */
  expires?: string | null;
}

export function storyFromWasm(story: WasmStory): Story {
  const postedMs = story.posted ? Date.parse(story.posted) : undefined;
  const expiresMs = story.expires ? Date.parse(story.expires) : undefined;
  return {
    id: story.id,
    ...(story.body ? { body: story.body } : {}),
    ...(story.media_url ? { mediaUrl: story.media_url } : {}),
    ...(story.author ? { author: story.author } : {}),
    ...(typeof postedMs === "number" && Number.isFinite(postedMs) ? { postedMs } : {}),
    ...(typeof expiresMs === "number" && Number.isFinite(expiresMs) ? { expiresMs } : {}),
  };
}

/** Returns true when the story is still active (no expiry, or expiry in the future). */
export function isStoryActive(story: Story, nowMs: number = Date.now()): boolean {
  if (typeof story.expiresMs !== "number") return true;
  return story.expiresMs > nowMs;
}

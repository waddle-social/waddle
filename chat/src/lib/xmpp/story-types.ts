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

export interface StoryReactionItem {
  /** Bare JID of the reacting member; also the XEP-0060 item id. */
  jid: string;
  emojis: readonly string[];
  /** XML children inside `<attachments/>` that this client does not understand. */
  unknownChildrenXml: readonly string[];
}

export interface StoryReactionSummary {
  counts: Record<string, number>;
  reactors: Record<string, string[]>;
  mine: readonly string[];
  noticedCount?: number;
}

export const NS_PUBSUB_ATTACHMENTS = "urn:xmpp:pubsub-attachments:1";
export const NS_PUBSUB_ATTACHMENTS_SUMMARY = "urn:xmpp:pubsub-attachments:summary:1";
export const STORIES_NODE = "urn:xmpp:stories:0";
export const STORY_REACTIONS_SUMMARY_NODE = `${NS_PUBSUB_ATTACHMENTS_SUMMARY}/urn:xmpp:stories:0`;
export const STORY_REACTIONS_MAX = 12;

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
  posted?: string | null;
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

export function storyAttachmentNode(communityJid: string, storyId: string): string {
  return `${NS_PUBSUB_ATTACHMENTS}/xmpp:${communityJid}?;node=${encodeURIComponent(STORIES_NODE)};item=${encodeURIComponent(storyId)}`;
}

export function normalizeStoryReactions(emojis: Iterable<string>): string[] {
  const seen = new Set<string>();
  const normalized: string[] = [];
  for (const raw of emojis) {
    const emoji = raw.trim();
    if (!emoji || seen.has(emoji)) continue;
    seen.add(emoji);
    normalized.push(emoji);
  }
  return normalized;
}

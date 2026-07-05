import { describe, expect, test } from "bun:test";
import type { FeedEntry } from "@/lib/xmpp/feed-types";
import { renderVueComponent } from "./helpers/render-vue-sfc";

/**
 * Regression for the shared-file XSS class (#1156) in the social-feed
 * surface: `urn:xmpp:pubsub-social-feed:0` entries are member-postable,
 * so `entry.link` is attacker-controlled and must not reach an anchor
 * `href` without the http/https allowlist (`safeExternalUrl`).
 */
function renderFeed(entries: FeedEntry[]): Promise<string> {
  return renderVueComponent(
    "../src/components/community/FeedPane.vue",
    {
      entries,
      stories: [],
      isLoading: false,
      isStoriesLoading: false,
      isPosting: false,
      isStoryPosting: false,
      error: null,
      storiesError: null,
      canPost: false,
      selfJid: "self@waddle.chat",
    },
    import.meta.url,
  );
}

function entry(link: string): FeedEntry {
  return { id: "e1", body: "hello", author: "mallory@waddle.chat", publishedMs: 1, link };
}

describe("FeedPane entry link sanitization", () => {
  test("javascript: link renders no anchor href", async () => {
    const html = await renderFeed([entry("javascript:alert(document.cookie)")]);
    expect(html).not.toContain("javascript:");
    expect(html).not.toContain('href="javascript');
  });

  test("data:text/html link renders no anchor href", async () => {
    const html = await renderFeed([entry("data:text/html,<script>alert(1)</script>")]);
    expect(html).not.toContain("data:text/html");
  });

  test("https link is preserved as the anchor href", async () => {
    const html = await renderFeed([entry("https://example.com/post")]);
    expect(html).toContain('href="https://example.com/post"');
  });
});

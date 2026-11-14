import { ref } from "vue";
import penguins from "@waddle/design/assets/story-penguins.svg?url";
import stickers from "@waddle/design/assets/spot-stickers.svg?url";
import spot from "@waddle/design/assets/spot-empty.svg?url";
import type { ChatAppController } from "@/shell/chat-app-controller";
import type { FeedEntry, Story, StoryReactionSummary } from "@/lib/xmpp-client";

/**
 * Preview fixtures for the feed screens:
 *   feed | feed-stories | feed-loading | feed-empty | feed-error | story-*
 * Composer and viewer states are reached by clicks in the shot script;
 * `window.__feedStoryPosting` flips the story upload busy state.
 */
export function seedFeed(c: ChatAppController, screen: string) {
  if (!screen.startsWith("feed") && !screen.startsWith("story")) return;
  const now = Date.now();
  const min = 60_000;
  const entries: FeedEntry[] = [
    { id: "p1", author: "mika@waddle.test", publishedMs: now - 20 * min, body: "0.9 release candidate is tagged. Thanks to everyone who tested MUC history this week." },
    { id: "p2", author: "jo@waddle.test", publishedMs: now - 60 * min, source: "tune", body: "Jo is listening to Hjaltalín · Feels Like Sugar" },
    { id: "p3", author: "priya@waddle.test", publishedMs: now - 120 * min, source: "mood", body: "Priya feels focused: \"shipping the Apple build\"" },
    { id: "p4", author: "kim@waddle.test", publishedMs: now - 200 * min, source: "rsvp", body: "Kim is going to Community meetup" },
  ];
  const stories: Story[] = [
    { id: "s1", author: "mika@waddle.test", postedMs: now - 42 * min, body: "RC tagged. Celebrating with the team huddle, cocoa and one very proud penguin.", mediaUrl: penguins },
    { id: "s2", author: "priya@waddle.test", postedMs: now - 300 * min, body: "Twenty seconds of the Apple build passing every check, set to the sound of my laptop fan.", mediaUrl: "https://waddle.test/media/build.mp4" },
    { id: "s3", author: "jo@waddle.test", postedMs: now - 180 * min, body: "Meetup stickers arrived. Grab some on the 18th.", mediaUrl: stickers },
    { id: "s4", author: "kim@waddle.test", postedMs: now - 540 * min, body: "New empty-state spot for rooms. Feedback welcome in #design.", mediaUrl: spot },
  ];
  const reactions: Record<string, StoryReactionSummary> = {
    s1: { counts: { "👍": 4, "🎉": 3 }, reactors: { "👍": ["Jo", "Priya", "Theo", "Sam"], "🎉": ["Jo", "Kim", "You"] }, mine: ["🎉"] },
    s2: { counts: { "😂": 2, "👀": 1 }, reactors: { "😂": ["Jo", "Theo"], "👀": ["You"] }, mine: ["👀"] },
    s3: { counts: { "❤️": 4 }, reactors: { "❤️": ["Mika", "Kim", "Theo", "Sam"] }, mine: [] },
  };
  const empty = screen === "feed-empty" || screen === "feed-loading";
  const feed = c.socialFeed as unknown as Record<string, unknown>;
  const story = c.stories as unknown as Record<string, unknown>;
  feed.entries = ref(empty || screen.startsWith("feed-stories") ? [] : entries);
  feed.isLoading = ref(screen === "feed-loading");
  feed.error = ref(screen === "feed-error" ? "The feed node timed out." : null);
  story.activeStories = ref(empty ? [] : stories);
  story.isLoading = ref(screen === "feed-loading");
  const posting = ref(false);
  story.isPosting = posting;
  Reflect.set(window, "__feedStoryPosting", posting);
  story.isStoryRead = (id: string) => id === "s3" || id === "s4";
  story.reactionSummary = (id: string) => reactions[id] ?? { counts: {}, reactors: {}, mine: [] };
  story.markStoryRead = () => {};
  story.toggleReaction = () => {};
  c.openCommunitySurface("feed");
  if (screen.startsWith("feed-stories") || screen.startsWith("story")) {
    c.ui.feedDefaultFilter.value = "stories";
    c.ui.feedDefaultComposerMode.value = "story";
  }
}

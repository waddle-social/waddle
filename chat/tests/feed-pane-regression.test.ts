import { describe, expect, test } from "bun:test";

const feedPaneSource = await Bun.file(
  new URL("../src/components/community/FeedPane.vue", import.meta.url),
).text();
const composerSource = await Bun.file(
  new URL("../src/components/community/FeedUpdateComposer.vue", import.meta.url),
).text();
const storyReaderSource = await Bun.file(
  new URL("../src/components/community/StoryReaderDialog.vue", import.meta.url),
).text();
const chatReadyShellSource = await Bun.file(
  new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url),
).text();
const shellStateSource = await Bun.file(
  new URL("../src/shell/state.ts", import.meta.url),
).text();
const shellControllerSource = await Bun.file(
  new URL("../src/shell/chat-app-controller.ts", import.meta.url),
).text();

function occurrences(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1;
}

describe("FeedPane composer regression", () => {
  test("renders a single feed create surface with all feed content modes", () => {
    expect(occurrences(feedPaneSource, "<FeedUpdateComposer")).toBe(1);
    expect(feedPaneSource).toContain(':publish-post="publishPost"');
    expect(feedPaneSource).toContain(':publish-story="publishStory"');
    expect(feedPaneSource).toContain(':initial-mode="initialComposerMode"');
    expect(composerSource).toContain('aria-label="Create feed update"');
    expect(composerSource).toContain('role="tablist" aria-label="Feed update type"');
    for (const mode of ["post", "story", "mood", "activity", "tune", "profile"]) {
      expect(composerSource).toContain(`id: "${mode}"`);
    }
  });

  test("does not keep the old separate story launcher", () => {
    expect(feedPaneSource).not.toContain("storyComposerOpen");
    expect(feedPaneSource).not.toContain("Add story");
    expect(composerSource).toContain("embedded");
  });

  test("resets the embedded story composer only after publish succeeds", () => {
    expect(composerSource).toContain(':key="storyComposerKey"');
    expect(composerSource).toContain("await props.publishStory");
    expect(composerSource).toContain("storyComposerKey.value += 1");
  });

  test("syncs /stories route intent into the unified composer", () => {
    expect(shellStateSource).toContain('feedDefaultFilter = ref<FeedSurfaceFilter>("all")');
    expect(shellStateSource).toContain('feedDefaultComposerMode = ref<FeedSurfaceComposerMode>("post")');
    expect(shellControllerSource).toContain('match.id === "stories"');
    expect(shellControllerSource).toContain('ui.feedDefaultFilter.value = "stories"');
    expect(shellControllerSource).toContain('ui.feedDefaultComposerMode.value = "story"');
    expect(chatReadyShellSource).toContain(':initial-filter="ui.feedDefaultFilter.value"');
    expect(chatReadyShellSource).toContain(':initial-composer-mode="ui.feedDefaultComposerMode.value"');
    expect(composerSource).toContain('initialMode?: FeedSurfaceComposerMode');
    expect(composerSource).toContain('const composerMode = ref<ComposerMode>(props.initialMode)');
  });

  test("does not leak story errors or stale profile loads across composer modes", () => {
    expect(composerSource).toContain("storyComposerError.value = null");
    expect(composerSource).toContain('composerMode === \'story\' && storyComposerError');
    expect(composerSource).toContain("function setComposerFeedback(mode: ComposerMode");
    expect(composerSource).toContain("if (composerMode.value !== mode) return");
    expect(composerSource).toContain('setComposerFeedback("story", "success"');
    expect(composerSource).toContain('if (composerMode.value === "story")');
    expect(composerSource).toContain("let profileLoadRequestId = 0");
    expect(composerSource).toContain("requestId !== profileLoadRequestId || selfJid !== props.selfJid");
    expect(composerSource).toContain("Load your saved profile before publishing changes.");
  });
});

describe("FeedPane story reader regression", () => {
  test("opens stories in a teleported dialog instead of an inline feed article", () => {
    expect(occurrences(feedPaneSource, "<StoryReaderDialog")).toBe(1);
    expect(storyReaderSource).toContain('<Teleport to="body">');
    expect(storyReaderSource).toContain('role="dialog"');
    expect(storyReaderSource).toContain('aria-modal="true"');
    expect(storyReaderSource).toContain('aria-label="Story"');
    expect(storyReaderSource).toContain('tabindex="-1"');
    expect(storyReaderSource).toContain("trapTab(event)");
    expect(storyReaderSource).toContain("eventTargetUsesArrows(event.target)");
    expect(feedPaneSource).not.toContain("v-if=\"activeStory && activeFilter");
  });

  test("tracks the active story by id so feed refreshes cannot retarget the modal", () => {
    expect(feedPaneSource).toContain("const activeStoryId = ref<string | null>(null)");
    expect(feedPaneSource).toContain("props.stories.findIndex((story) => story.id === id)");
    expect(feedPaneSource).toContain("activeStoryId.value = story.id");
    expect(feedPaneSource).not.toContain("activeStoryIndex = ref");
  });
});

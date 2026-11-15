<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Film, Menu, RefreshCw, SmilePlus } from "lucide-vue-next";
import spot from "@waddle/design/assets/spot-empty.svg?url";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import Skeleton from "@/components/ui/Skeleton.vue";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import FeedUpdateComposer from "@/components/community/FeedUpdateComposer.vue";
import StoryReaderDialog from "@/components/community/StoryReaderDialog.vue";
import type { FeedSurfaceComposerMode, FeedSurfaceFilter } from "@/shell/state";
import type { ActivityPublication, MoodPublication, TunePublication } from "@/lib/xmpp/pep-types";
import type { VCard4Profile } from "@/lib/xmpp/vcard4-types";
import type { FeedEntry, FeedPostInput, FeedSourceKind, Story, StoryPostInput, StoryReactionSummary } from "@/lib/xmpp-client";
import { jidLocalpart } from "@/lib/xmpp/jid";
import { safeExternalUrl } from "@/lib/chat-ui";
import { css, cx } from "styled-system/css";
import { btn, chip, empty, floe, iconbtn, seg } from "styled-system/recipes";

const SOURCE_LABELS: Record<FeedSourceKind, string> = {
  mood: "Mood",
  activity: "Activity",
  tune: "Listening",
  avatar: "New avatar",
  vcard: "Profile update",
  rsvp: "Event RSVP",
};

const ui = {
  scroll: css({ flex: 1, minHeight: 0, overflowX: "hidden", overflowY: "auto", scrollbarGutter: "stable", bg: "bg", color: "fg" }),
  main: css({
    boxSizing: "border-box",
    display: "flex",
    flexDirection: "column",
    gap: "6",
    width: "100%",
    maxWidth: "1000px",
    padding: { base: "14px 12px 32px", md: "32px 40px" },
  }),
  bar: css({
    display: { base: "flex", md: "none" },
    position: "sticky",
    top: 0,
    zIndex: "sticky",
    alignItems: "center",
    gap: "1.5",
    padding: "2",
    borderBottom: "1px solid {colors.border.soft}",
    bg: "bg.rail",
    color: "fg",
    "& b": { flex: 1, minWidth: 0, font: "700 18px/1.2 {fonts.display}" },
    "& .wd-iconbtn": { width: "44px", height: "44px" },
  }),
  head: css({ display: { base: "none", md: "flex" }, alignItems: "flex-end", gap: "4", flexWrap: "wrap" }),
  headText: css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "1.5" }),
  title: css({ margin: 0, font: "700 30px/1.2 {fonts.display}", letterSpacing: "-0.01em" }),
  sub: css({ margin: 0, fontSize: "15px", color: "fg.muted" }),
  filters: cx(seg(), css({ alignSelf: "flex-start", maxWidth: "100%", overflowX: "auto", flex: "none" })),
  count: css({ fontFamily: "mono", fontSize: "11px", color: "fg.muted", fontVariantNumeric: "tabular-nums" }),
  alert: css({ padding: "10px 12px", borderRadius: "10px", bg: "danger.subtle", border: "1px solid {colors.danger}", color: "danger.text", fontSize: "13px" }),
  list: css({ display: "flex", flexDirection: "column", gap: "3" }),
  card: cx(floe(), css({ "--pad": "16px", gap: "2.5" })),
  who: css({ display: "flex", alignItems: "center", gap: "2.5", minWidth: 0 }),
  whoText: css({ display: "flex", flexDirection: "column", lineHeight: 1.3, minWidth: 0, "& b": { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" } }),
  caption: css({ textStyle: "caption", color: "fg.muted" }),
  entryTitle: css({ margin: 0, font: "700 17px/1.3 {fonts.display}" }),
  body: css({ margin: 0, fontSize: "15px", lineHeight: 1.5, whiteSpace: "pre-wrap", overflowWrap: "anywhere" }),
  link: css({ alignSelf: "flex-start", fontSize: "13px", color: "action.text", overflowWrap: "anywhere" }),
  story: css({
    position: "relative",
    boxSizing: "border-box",
    display: "grid",
    gridTemplateColumns: { base: "minmax(0, 1fr)", md: "minmax(0, 1fr) 128px" },
    gridTemplateAreas: { base: '"head" "media" "body" "foot"', md: '"head media" "body media" "foot media"' },
    gridTemplateRows: { md: "auto auto 1fr" },
    columnGap: "4",
    rowGap: "2.5",
    padding: { base: "14px", md: "16px" },
    borderRadius: "panel",
    bg: "surface",
    border: "1px solid {colors.border.soft}",
    overflow: "hidden",
  }),
  open: css({
    position: "absolute",
    inset: 0,
    border: 0,
    borderRadius: "inherit",
    bg: "transparent",
    cursor: "pointer",
    _hover: { bg: "state.hover" },
    _focusVisible: { outlineOffset: "-3px" },
  }),
  storyHead: css({ gridArea: "head", display: "flex", alignItems: "center", gap: "2", flexWrap: "wrap", minWidth: 0, "& b": { fontSize: "14px" } }),
  newChip: css({ display: "inline-flex", alignItems: "center", height: "22px", px: "2", borderRadius: "pill", bg: "gold", color: "gold.fg", font: "700 12px/1 {fonts.body}" }),
  storyBody: css({ gridArea: "body", margin: 0, fontSize: "15px", lineHeight: 1.5, overflowWrap: "anywhere", display: "-webkit-box", WebkitLineClamp: 3, WebkitBoxOrient: "vertical", overflow: "hidden" }),
  thumb: css({
    gridArea: "media",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    width: { base: "100%", md: "128px" },
    height: { md: "128px" },
    aspectRatio: { base: "16 / 9", md: "auto" },
    borderRadius: "control",
    overflow: "hidden",
    bg: "sea.shallow",
    "& img": { width: "88%", height: "88%", objectFit: "contain" },
  }),
  video: css({ display: "flex", flexDirection: "column", alignItems: "center", gap: "1.5", color: "fg.muted", fontSize: "13px", fontWeight: 600, "& svg": { width: "28px", height: "28px" } }),
  videoThumb: css({ bg: "surface.2" }),
  storyFoot: css({ gridArea: "foot", alignSelf: "end", display: "flex", alignItems: "center", gap: "1", flexWrap: "wrap", "& > button": { position: "relative" } }),
  reaction: chip(),
  skel: cx(floe(), css({ "--pad": "16px", gap: "3" })),
  skelRow: css({ display: "flex", alignItems: "center", gap: "3" }),
  skelText: css({ flex: 1, display: "flex", flexDirection: "column", gap: "1.5" }),
  empty: cx(empty(), css({ pt: "10", "& h2": { margin: "{spacing.2} 0 0", font: "700 24px/1.2 {fonts.display}" } })),
};
const refreshClass = btn({ size: "sm" });
const iconClass = iconbtn();

function labelFor(source: FeedSourceKind | undefined): string {
  return source ? SOURCE_LABELS[source] : "Post";
}

interface FeedPaneProps {
  entries: readonly FeedEntry[];
  stories: readonly Story[];
  isLoading: boolean;
  isStoriesLoading: boolean;
  isPosting: boolean;
  isStoryPosting: boolean;
  error: string | null;
  storiesError: string | null;
  canPost: boolean;
  selfJid: string | null;
  isStoryRead?: (id: string) => boolean;
  reactionSummary?: (id: string) => StoryReactionSummary;
  initialFilter?: FeedSurfaceFilter;
  initialComposerMode?: FeedSurfaceComposerMode;
  publishPost?: (input: FeedPostInput) => Promise<unknown>;
  publishStory?: (input: StoryPostInput) => Promise<unknown>;
  publishMood?: (input: MoodPublication) => Promise<void>;
  publishActivity?: (input: ActivityPublication) => Promise<void>;
  publishTune?: (input: TunePublication) => Promise<void>;
  fetchProfile?: () => Promise<VCard4Profile | null>;
  publishProfile?: (input: VCard4Profile) => Promise<void>;
}

const props = withDefaults(defineProps<FeedPaneProps>(), {
  isStoryRead: (_id: string) => false,
  reactionSummary: (_id: string) => ({ counts: {}, reactors: {}, mine: [] }),
  initialFilter: "all",
  initialComposerMode: "post",
});
const emit = defineEmits<{
  refresh: [];
  storySelected: [id: string];
  react: [id: string, emoji: string];
  openNav: [];
}>();

const activeStoryId = ref<string | null>(null);
const activeFilter = ref<FeedSurfaceFilter>(props.initialFilter);

watch(activeFilter, (filter) => {
  if (filter === "pep" || filter === "posts") {
    activeStoryId.value = null;
    closeStoryReactionPicker({ restoreFocus: false });
  }
});

watch(() => props.initialFilter, (filter) => {
  activeFilter.value = filter;
});

watch(() => props.stories, (stories) => {
  const id = activeStoryId.value;
  if (id && !stories.some((story) => story.id === id)) {
    activeStoryId.value = null;
  }
  const pickerStoryId = reactionPickerStoryId.value;
  if (pickerStoryId && !stories.some((story) => story.id === pickerStoryId)) {
    closeStoryReactionPicker({ restoreFocus: false });
  }
});

function timeAgo(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 60_000) return "just now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h ago`;
  if (delta < 604_800_000) return `${Math.floor(delta / 86_400_000)}d ago`;
  return new Date(ms).toLocaleDateString();
}

function longTimeAgo(ms: number): string {
  const delta = Math.max(0, Date.now() - ms);
  if (delta < 60_000) return "just now";
  if (delta < 3_600_000) return `${plural(Math.floor(delta / 60_000), "minute")} ago`;
  if (delta < 86_400_000) return `${plural(Math.floor(delta / 3_600_000), "hour")} ago`;
  if (delta < 604_800_000) return `${plural(Math.floor(delta / 86_400_000), "day")} ago`;
  return new Date(ms).toLocaleDateString();
}

function plural(value: number, unit: string): string {
  return `${value} ${unit}${value === 1 ? "" : "s"}`;
}

function listedAgo(story: Story): string {
  return typeof story.postedMs === "number" ? longTimeAgo(story.postedMs) : "";
}

function authorLabel(author: string | undefined): string {
  if (!author) return "Anonymous";
  return jidLocalpart(author);
}

type FeedItem =
  | { kind: "entry"; id: string; sortMs: number; entry: FeedEntry }
  | { kind: "story"; id: string; sortMs: number; story: Story; storyIndex: number };

const feedItems = computed<FeedItem[]>(() => {
  const entryItems = props.entries
    .filter((entry) => {
      if (activeFilter.value === "stories") return false;
      if (activeFilter.value === "pep") return Boolean(entry.source);
      if (activeFilter.value === "posts") return !entry.source;
      return true;
    })
    .map((entry) => ({
      kind: "entry" as const,
      id: `entry:${entry.id}`,
      sortMs: entry.publishedMs ?? 0,
      entry,
    }));
  const storyItems = activeFilter.value === "posts" || activeFilter.value === "pep"
    ? []
    : props.stories.map((story, storyIndex) => ({
      kind: "story" as const,
      id: `story:${story.id}`,
      sortMs: story.postedMs ?? 0,
      story,
      storyIndex,
    }));
  return [...entryItems, ...storyItems].sort((a, b) => b.sortMs - a.sortMs);
});

const filters = computed(() => [
  { id: "all" as const, label: "All", count: props.entries.length + props.stories.length },
  { id: "posts" as const, label: "Posts", count: props.entries.filter((entry) => !entry.source).length },
  { id: "stories" as const, label: "Stories", count: props.stories.length },
  { id: "pep" as const, label: "Status updates", count: props.entries.filter((entry) => entry.source).length },
]);

const activeStoryIndex = computed<number | null>(() => {
  const id = activeStoryId.value;
  if (!id) return null;
  const index = props.stories.findIndex((story) => story.id === id);
  return index >= 0 ? index : null;
});

const activeStory = computed<Story | null>(() => {
  const index = activeStoryIndex.value;
  return index === null ? null : props.stories[index] ?? null;
});

interface StoryReactionChip {
  emoji: string;
  count: number;
  mine: boolean;
  reactors: readonly string[];
}

/** Sorted, display-ready reaction chips for a story, highest count first. */
function reactionChipsFor(storyId: string): StoryReactionChip[] {
  const summary = props.reactionSummary(storyId);
  return Object.entries(summary.counts)
    .map(([emoji, count]) => ({ emoji, count, mine: summary.mine.includes(emoji), reactors: summary.reactors[emoji] ?? [] }))
    .sort((a, b) => b.count - a.count || a.emoji.localeCompare(b.emoji));
}

const activeReactionSummary = computed(() => activeStory.value ? props.reactionSummary(activeStory.value.id) : null);
const activeReactionEntries = computed(() => activeStory.value ? reactionChipsFor(activeStory.value.id) : []);

const loadingAny = computed(() => props.isLoading || props.isStoriesLoading);
const emptyCopy = computed(() => {
  if (activeFilter.value === "stories") return props.canPost ? "Add the first story here." : "Check back later.";
  if (activeFilter.value === "pep") return "Profile, mood, activity, tune, and RSVP updates land here.";
  if (activeFilter.value === "posts") return props.canPost ? "Share the first community post." : "Check back later.";
  return props.canPost ? "Share the first update. Status updates, stories, and posts land here." : "Check back later.";
});

function isVideoStory(story: Story): boolean {
  return Boolean(story.mediaUrl && /\.(mp4|webm|mov)(\?|$)/i.test(story.mediaUrl));
}

function selectStory(index: number) {
  const story = props.stories[index];
  if (!story?.id) return;
  activeStoryId.value = story.id;
  emit("storySelected", story.id);
}

function closeReader() {
  activeStoryId.value = null;
}

function prevStory() {
  const index = activeStoryIndex.value;
  if (index === null || index <= 0) return;
  selectStory(index - 1);
}

function nextStory() {
  const index = activeStoryIndex.value;
  if (index === null || index >= props.stories.length - 1) return;
  selectStory(index + 1);
}

function reactToStory(storyId: string, emoji: string) {
  if (!storyId) return;
  emit("react", storyId, emoji);
}

function toggleReaction(emoji: string) {
  if (!activeStory.value) return;
  reactToStory(activeStory.value.id, emoji);
}

const reactionPickerStoryId = ref<string | null>(null);
const reactionPickerAnchor = ref<HTMLElement | null>(null);

function openStoryReactionPicker(storyId: string, event: MouseEvent) {
  if (reactionPickerStoryId.value === storyId) {
    closeStoryReactionPicker();
    return;
  }
  reactionPickerAnchor.value = event.currentTarget as HTMLElement;
  reactionPickerStoryId.value = storyId;
}

function closeStoryReactionPicker({ restoreFocus = true } = {}) {
  const anchor = reactionPickerAnchor.value;
  reactionPickerStoryId.value = null;
  reactionPickerAnchor.value = null;
  // Return focus to the trigger so keyboard users don't get dropped to <body>.
  // Skip for programmatic dismissals (filter change / story removal): the
  // anchor is about to leave the DOM, so focusing it would steal focus from
  // wherever the user currently is and then drop it to <body> on re-render.
  if (restoreFocus) anchor?.focus();
}

function selectStoryReaction(emoji: string) {
  const storyId = reactionPickerStoryId.value;
  if (!storyId) return;
  reactToStory(storyId, emoji);
  closeStoryReactionPicker();
}
</script>

<template>
  <div :class="ui.scroll">
    <header :class="ui.bar" data-theme="night">
      <button type="button" :class="iconClass" aria-label="Open navigation" @click="emit('openNav')">
        <Menu aria-hidden="true" />
      </button>
      <b>Feed</b>
      <button
        type="button"
        :class="iconClass"
        :disabled="loadingAny"
        :aria-label="loadingAny ? 'Refreshing' : 'Refresh feed'"
        @click="emit('refresh')"
      >
        <RefreshCw aria-hidden="true" />
      </button>
    </header>

    <div :class="ui.main">
      <header :class="ui.head">
        <div :class="ui.headText">
          <h1 :class="ui.title">Feed</h1>
          <p :class="ui.sub">What people across the community are sharing</p>
        </div>
        <button
          type="button"
          :class="refreshClass"
          :disabled="loadingAny"
          :aria-label="loadingAny ? 'Refreshing' : 'Refresh feed'"
          @click="emit('refresh')"
        >
          <RefreshCw aria-hidden="true" />
          Refresh
        </button>
      </header>

      <FeedUpdateComposer
        v-if="canPost"
        :self-jid="selfJid"
        :is-posting="isPosting"
        :is-story-posting="isStoryPosting"
        :publish-post="publishPost"
        :publish-story="publishStory"
        :initial-mode="initialComposerMode"
        :publish-mood="publishMood"
        :publish-activity="publishActivity"
        :publish-tune="publishTune"
        :fetch-profile="fetchProfile"
        :publish-profile="publishProfile"
      />

      <div :class="ui.filters" role="group" aria-label="Feed filters">
        <button
          v-for="filter in filters"
          :key="filter.id"
          type="button"
          :aria-pressed="activeFilter === filter.id ? 'true' : 'false'"
          @click="activeFilter = filter.id"
        >
          {{ filter.label }}
          <span :class="ui.count">{{ filter.count }}</span>
        </button>
      </div>

      <div v-if="error" :class="ui.alert">
        Couldn't load the feed: {{ error }}
      </div>
      <div v-if="storiesError" :class="ui.alert">
        Couldn't load stories: {{ storiesError }}
      </div>
      <div :class="ui.list">
        <template v-for="item in feedItems" :key="item.id">
          <article v-if="item.kind === 'entry'" :class="ui.card">
            <header :class="ui.who">
              <UserAvatar :name="authorLabel(item.entry.author)" :jid="item.entry.author ?? null" size="md" />
              <div :class="ui.whoText">
                <b>{{ authorLabel(item.entry.author) }}</b>
                <span :class="ui.caption">
                  {{ labelFor(item.entry.source) }}<template v-if="item.entry.publishedMs"> · {{ timeAgo(item.entry.publishedMs) }}</template>
                </span>
              </div>
            </header>
            <h2 v-if="item.entry.title" :class="ui.entryTitle">
              {{ item.entry.title }}
            </h2>
            <p :class="ui.body">{{ item.entry.body }}</p>
            <a
              v-if="safeExternalUrl(item.entry.link)"
              :href="safeExternalUrl(item.entry.link)"
              target="_blank"
              rel="noopener noreferrer"
              :class="ui.link"
            >
              {{ item.entry.link }}
            </a>
          </article>
          <article v-else :class="ui.story" :aria-label="`Story from ${authorLabel(item.story.author)}`">
            <button
              type="button"
              :class="ui.open"
              :aria-label="`Open story from ${authorLabel(item.story.author)}`"
              @click="selectStory(item.storyIndex)"
            />
            <div :class="ui.storyHead">
              <UserAvatar :name="authorLabel(item.story.author)" :jid="item.story.author ?? null" size="sm" />
              <b>{{ authorLabel(item.story.author) }}</b>
              <span v-if="!isStoryRead(item.story.id)" :class="ui.newChip">New</span>
              <span v-if="listedAgo(item.story)" :class="ui.caption">{{ listedAgo(item.story) }}</span>
            </div>
            <p v-if="item.story.body" :class="ui.storyBody">{{ item.story.body }}</p>
            <span
              v-if="item.story.mediaUrl"
              :class="[ui.thumb, isVideoStory(item.story) && ui.videoThumb]"
            >
              <span v-if="isVideoStory(item.story)" :class="ui.video">
                <Film aria-hidden="true" />
                Video
              </span>
              <img
                v-else
                :src="item.story.mediaUrl"
                :alt="`Story media from ${authorLabel(item.story.author)}`"
                referrerpolicy="no-referrer"
              />
            </span>
            <footer :class="ui.storyFoot">
              <button
                v-for="chip in reactionChipsFor(item.story.id)"
                :key="chip.emoji"
                type="button"
                :class="ui.reaction"
                :aria-label="`React with ${chip.emoji}, ${chip.count} so far`"
                :aria-pressed="chip.mine"
                @click="reactToStory(item.story.id, chip.emoji)"
              >
                <span aria-hidden="true">{{ chip.emoji }}</span>
                <span class="wd-chip__n">{{ chip.count }}</span>
              </button>
              <button
                type="button"
                :class="iconClass"
                :aria-expanded="reactionPickerStoryId === item.story.id"
                aria-haspopup="dialog"
                aria-label="React to story"
                @click="openStoryReactionPicker(item.story.id, $event)"
              >
                <SmilePlus aria-hidden="true" />
              </button>
            </footer>
          </article>
        </template>

        <div v-if="loadingAny && feedItems.length === 0" :class="ui.list" aria-busy="true" aria-label="Loading feed">
          <div v-for="i in 3" :key="`feed-skel-${i}`" :class="ui.skel" aria-hidden="true">
            <div :class="ui.skelRow">
              <Skeleton width="28px" height="28px" radius="9999px" />
              <div :class="ui.skelText">
                <Skeleton width="35%" height="0.875rem" />
              </div>
            </div>
            <Skeleton width="60%" height="0.75rem" />
            <Skeleton width="45%" height="0.75rem" />
          </div>
        </div>
        <div v-else-if="feedItems.length === 0" role="status" :class="ui.empty">
          <img :src="spot" alt="" />
          <h2>It is quiet in here.</h2>
          <p>{{ emptyCopy }}</p>
        </div>
      </div>
    </div>
  </div>

  <StoryReaderDialog
    :story="activeStory"
    :story-index="activeStoryIndex"
    :story-count="stories.length"
    :reaction-summary="activeReactionSummary"
    :reaction-entries="activeReactionEntries"
    @close="closeReader"
    @previous="prevStory"
    @next="nextStory"
    @react="toggleReaction"
  />

  <EmojiPicker
    :open="reactionPickerStoryId !== null"
    :anchor-el="reactionPickerAnchor"
    @select="selectStoryReaction"
    @close="closeStoryReactionPicker"
  />
</template>

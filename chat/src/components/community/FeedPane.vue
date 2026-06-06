<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Briefcase,
  CalendarCheck,
  Camera,
  Film,
  IdCard,
  Inbox,
  Menu,
  MessageSquareText,
  Music,
  RefreshCw,
  Smile,
  User,
} from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import Skeleton from "@/components/ui/Skeleton.vue";
import FeedUpdateComposer from "@/components/community/FeedUpdateComposer.vue";
import StoryReaderDialog from "@/components/community/StoryReaderDialog.vue";
import type { FeedSurfaceComposerMode, FeedSurfaceFilter } from "@/shell/state";
import type { ActivityPublication, MoodPublication, TunePublication } from "@/lib/xmpp/pep-types";
import type { VCard4Profile } from "@/lib/xmpp/vcard4-types";
import type { FeedEntry, FeedPostInput, FeedSourceKind, Story, StoryPostInput, StoryReactionSummary } from "@/lib/xmpp-client";

const SOURCE_ICONS = {
  mood: Smile,
  activity: Briefcase,
  tune: Music,
  avatar: User,
  vcard: IdCard,
  rsvp: CalendarCheck,
} as const;

const SOURCE_LABELS: Record<FeedSourceKind, string> = {
  mood: "Mood update",
  activity: "Activity",
  tune: "Now listening",
  avatar: "New avatar",
  vcard: "Profile update",
  rsvp: "Event RSVP",
};

/**
 * Per-kind accent palette. Tailwind utility classes (text + bg) are
 * paired so the badge and the card's left accent line stay tonally
 * consistent. Chosen for contrast in both light and dark theme.
 */
const SOURCE_ACCENT: Record<FeedSourceKind, { chip: string; rail: string }> = {
  mood: {
    chip: "bg-amber-500/15 text-amber-700 dark:text-amber-300 ring-amber-500/20",
    rail: "bg-amber-500/70",
  },
  activity: {
    chip: "bg-sky-500/15 text-sky-700 dark:text-sky-300 ring-sky-500/20",
    rail: "bg-sky-500/70",
  },
  tune: {
    chip: "bg-violet-500/15 text-violet-700 dark:text-violet-300 ring-violet-500/20",
    rail: "bg-violet-500/70",
  },
  avatar: {
    chip: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300 ring-emerald-500/20",
    rail: "bg-emerald-500/70",
  },
  vcard: {
    chip: "bg-rose-500/15 text-rose-700 dark:text-rose-300 ring-rose-500/20",
    rail: "bg-rose-500/70",
  },
  rsvp: {
    chip: "bg-indigo-500/15 text-indigo-700 dark:text-indigo-300 ring-indigo-500/20",
    rail: "bg-indigo-500/70",
  },
};

function iconFor(source: FeedSourceKind | undefined) {
  return source ? SOURCE_ICONS[source] : null;
}

function labelFor(source: FeedSourceKind | undefined): string {
  return source ? SOURCE_LABELS[source] : "";
}

function chipClassFor(source: FeedSourceKind | undefined): string {
  return source ? SOURCE_ACCENT[source].chip : "";
}

function railClassFor(source: FeedSourceKind | undefined): string {
  return source ? SOURCE_ACCENT[source].rail : "bg-border";
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
  isStoryRead: () => () => false,
  reactionSummary: () => () => ({ counts: {}, reactors: {}, mine: [] }),
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
  return author.split("@")[0] ?? author;
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
  { id: "pep" as const, label: "PEP updates", count: props.entries.filter((entry) => entry.source).length },
  { id: "stories" as const, label: "Stories", count: props.stories.length },
  { id: "posts" as const, label: "Posts", count: props.entries.filter((entry) => !entry.source).length },
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

const activeReactionSummary = computed(() => activeStory.value ? props.reactionSummary(activeStory.value.id) : null);
const activeReactionEntries = computed(() => {
  const summary = activeReactionSummary.value;
  if (!summary) return [];
  return Object.entries(summary.counts)
    .map(([emoji, count]) => ({ emoji, count, reactors: summary.reactors[emoji] ?? [] }))
    .sort((a, b) => b.count - a.count || a.emoji.localeCompare(b.emoji));
});

const loadingAny = computed(() => props.isLoading || props.isStoriesLoading);
const emptyCopy = computed(() => {
  if (activeFilter.value === "stories") return props.canPost ? "Add the first story here." : "Check back later.";
  if (activeFilter.value === "pep") return "Profile, mood, activity, tune, and RSVP updates land here.";
  if (activeFilter.value === "posts") return props.canPost ? "Share the first community post." : "Check back later.";
  return props.canPost ? "Share the first update. PEP updates, stories, and posts land here." : "Check back later.";
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

function toggleReaction(emoji: string) {
  const story = activeStory.value;
  if (!story) return;
  emit("react", story.id, emoji);
}
</script>

<template>
  <div class="chat-pane-scroll flex-1 min-h-0 bg-background px-[var(--chat-content-inline)] py-6">
    <div class="mx-auto grid w-full max-w-2xl gap-5">
      <header class="flex items-center gap-2">
        <button
          type="button"
          class="md:hidden inline-flex h-8 w-8 items-center justify-center rounded-md border border-transparent text-muted-foreground hover:bg-muted/50 hover:text-foreground"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Menu class="h-4 w-4" aria-hidden="true" />
        </button>
        <span class="flex h-9 w-9 items-center justify-center rounded-full bg-primary/10 text-primary">
          <MessageSquareText class="h-4.5 w-4.5" aria-hidden="true" />
        </span>
        <div class="min-w-0">
          <h1 class="type-pane-title text-foreground leading-tight">Community Feed</h1>
          <p class="type-caption text-muted-foreground">Highlights from across the community</p>
        </div>
        <button
          type="button"
          class="ml-auto inline-flex items-center gap-1.5 rounded-md border border-transparent px-2 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
          :disabled="loadingAny"
          :aria-label="loadingAny ? 'Refreshing' : 'Refresh feed'"
          @click="emit('refresh')"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loadingAny }" aria-hidden="true" />
          <span class="hidden sm:inline">Refresh</span>
        </button>
      </header>

      <div class="flex flex-wrap items-center gap-1.5" role="group" aria-label="Feed filters">
        <button
          v-for="filter in filters"
          :key="filter.id"
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1.5 text-xs transition-colors"
          :class="activeFilter === filter.id ? 'border-primary bg-primary/10 text-primary' : 'border-input text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
          :aria-pressed="activeFilter === filter.id ? 'true' : 'false'"
          @click="activeFilter = filter.id"
        >
          {{ filter.label }}
          <span class="text-[0.65rem] opacity-75">{{ filter.count }}</span>
        </button>
      </div>

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

      <div
        v-if="error"
        class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        Couldn't load the feed: {{ error }}
      </div>
      <div
        v-if="storiesError"
        class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        Couldn't load stories: {{ storiesError }}
      </div>
      <div class="grid gap-3">
        <article
          v-for="item in feedItems"
          :key="item.id"
          class="group relative overflow-hidden rounded-xl border border-border bg-card shadow-sm transition-colors hover:bg-card/80"
        >
          <span
            class="absolute inset-y-0 left-0 w-1"
            :class="item.kind === 'story' ? (isStoryRead(item.story.id) ? 'bg-border' : 'bg-primary/70') : railClassFor(item.entry.source)"
            aria-hidden="true"
          ></span>
          <div v-if="item.kind === 'entry'" class="grid gap-2 pl-4 pr-4 py-3.5">
            <header class="flex min-w-0 items-start gap-3">
              <AppAvatar :name="authorLabel(item.entry.author)" :src="null" size="md" />
              <div class="min-w-0 flex-1">
                <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <p class="type-control truncate font-semibold text-foreground">
                    {{ authorLabel(item.entry.author) }}
                  </p>
                  <span
                    v-if="item.entry.source"
                    class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[0.65rem] font-medium uppercase tracking-wide ring-1 ring-inset"
                    :class="chipClassFor(item.entry.source)"
                    :aria-label="labelFor(item.entry.source)"
                  >
                    <component
                      :is="iconFor(item.entry.source)"
                      class="h-3 w-3"
                      aria-hidden="true"
                    />
                    {{ labelFor(item.entry.source) }}
                  </span>
                </div>
                <p
                  v-if="item.entry.publishedMs"
                  class="type-caption text-muted-foreground"
                  :title="new Date(item.entry.publishedMs).toLocaleString()"
                >
                  {{ timeAgo(item.entry.publishedMs) }}
                </p>
              </div>
            </header>
            <h2
              v-if="item.entry.title"
              class="type-control font-semibold text-foreground"
            >
              {{ item.entry.title }}
            </h2>
            <p class="whitespace-pre-wrap break-words text-sm leading-relaxed text-foreground">
              {{ item.entry.body }}
            </p>
            <a
              v-if="item.entry.link"
              :href="item.entry.link"
              target="_blank"
              rel="noopener noreferrer"
              class="type-caption inline-flex items-center gap-1 text-primary underline-offset-2 hover:underline"
            >
              {{ item.entry.link }}
            </a>
          </div>
          <button
            v-else
            type="button"
            class="grid w-full gap-3 p-4 text-left sm:grid-cols-[minmax(0,1fr)_8rem] sm:items-start"
            @click="selectStory(item.storyIndex)"
          >
            <span class="grid min-w-0 gap-3">
              <span class="flex min-w-0 items-start gap-3">
                <AppAvatar :name="authorLabel(item.story.author)" :src="null" size="md" />
                <span class="min-w-0 flex-1">
                  <span class="flex flex-wrap items-center gap-x-2 gap-y-1">
                    <span class="type-control truncate font-semibold text-foreground">
                      {{ authorLabel(item.story.author) }}
                    </span>
                    <span class="inline-flex items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 text-[0.65rem] font-medium uppercase text-primary ring-1 ring-inset ring-primary/20">
                      <Camera class="h-3 w-3" aria-hidden="true" />
                      Story
                    </span>
                    <span
                      v-if="!isStoryRead(item.story.id)"
                      class="rounded-full bg-primary/10 px-2 py-0.5 text-[0.65rem] font-medium uppercase text-primary ring-1 ring-inset ring-primary/20"
                    >
                      New
                    </span>
                  </span>
                  <span
                    v-if="listedAgo(item.story)"
                    class="type-caption block text-muted-foreground"
                    :title="item.story.postedMs ? new Date(item.story.postedMs).toLocaleString() : undefined"
                  >
                    {{ listedAgo(item.story) }}
                  </span>
                </span>
              </span>
              <span v-if="item.story.body" class="line-clamp-3 whitespace-pre-wrap break-words text-sm leading-relaxed text-foreground">
                {{ item.story.body }}
              </span>
            </span>
            <span
              v-if="item.story.mediaUrl"
              class="flex aspect-video w-full items-center justify-center overflow-hidden rounded-md bg-muted text-muted-foreground sm:aspect-square"
            >
              <span v-if="isVideoStory(item.story)" class="inline-flex items-center gap-1 text-xs">
                <Film class="h-4 w-4" aria-hidden="true" />
                Video
              </span>
              <img
                v-else
                :src="item.story.mediaUrl"
                :alt="`Story media from ${authorLabel(item.story.author)}`"
                class="h-full w-full object-cover"
                referrerpolicy="no-referrer"
              />
            </span>
          </button>
        </article>

        <template v-if="loadingAny && feedItems.length === 0">
          <div
            v-for="i in 3"
            :key="`feed-skel-${i}`"
            class="grid gap-3 rounded-xl border border-border bg-card px-4 py-3.5 shadow-sm"
            aria-hidden="true"
          >
            <div class="flex items-center gap-3">
              <Skeleton width="2.25rem" height="2.25rem" radius="9999px" />
              <div class="flex flex-1 flex-col gap-1.5">
                <Skeleton width="35%" height="0.75rem" />
                <Skeleton width="20%" height="0.6rem" />
              </div>
            </div>
            <Skeleton width="100%" height="0.8rem" />
            <Skeleton width="80%" height="0.8rem" />
          </div>
        </template>
        <div
          v-else-if="feedItems.length === 0"
          class="flex flex-col items-center gap-2 rounded-xl border border-dashed border-border bg-card/40 px-6 py-10 text-center"
        >
          <span class="flex h-10 w-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
            <Inbox class="h-5 w-5" aria-hidden="true" />
          </span>
          <p class="type-control text-foreground">It's quiet in here</p>
          <p class="type-caption text-muted-foreground">
            {{ emptyCopy }}
          </p>
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
</template>

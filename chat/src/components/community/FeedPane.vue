<script setup lang="ts">
import { computed, ref } from "vue";
import {
  Briefcase,
  CalendarCheck,
  IdCard,
  Inbox,
  Menu,
  MessageSquareText,
  Music,
  RefreshCw,
  Send,
  Smile,
  User,
} from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import Skeleton from "@/components/ui/Skeleton.vue";
import type { FeedEntry, FeedPostInput, FeedSourceKind } from "@/lib/xmpp-client";

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
  isLoading: boolean;
  isPosting: boolean;
  error: string | null;
  canPost: boolean;
  selfJid: string | null;
}

const props = defineProps<FeedPaneProps>();
const emit = defineEmits<{
  refresh: [];
  post: [input: FeedPostInput];
  openNav: [];
}>();

const composerBody = ref("");
const COMPOSER_MAX = 500;

function timeAgo(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 60_000) return "just now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h ago`;
  if (delta < 604_800_000) return `${Math.floor(delta / 86_400_000)}d ago`;
  return new Date(ms).toLocaleDateString();
}

function authorLabel(author: string | undefined): string {
  if (!author) return "Anonymous";
  return author.split("@")[0] ?? author;
}

const canSubmit = computed(() => {
  return props.canPost && !props.isPosting && composerBody.value.trim().length > 0;
});

const composerOver = computed(() => composerBody.value.length > COMPOSER_MAX);

function submit() {
  const body = composerBody.value.trim();
  if (!body || composerOver.value) return;
  emit("post", { body, ...(props.selfJid ? { author: props.selfJid.split("/")[0] } : {}) });
  composerBody.value = "";
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
          :disabled="isLoading"
          :aria-label="isLoading ? 'Refreshing' : 'Refresh feed'"
          @click="emit('refresh')"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': isLoading }" aria-hidden="true" />
          <span class="hidden sm:inline">Refresh</span>
        </button>
      </header>

      <form
        v-if="canPost"
        class="grid gap-3 rounded-xl border border-border bg-card p-4 shadow-sm focus-within:ring-1 focus-within:ring-ring/40"
        @submit.prevent="submit"
      >
        <div class="flex items-start gap-3">
          <AppAvatar :name="authorLabel(selfJid ?? '')" :src="null" size="md" />
          <textarea
            v-model="composerBody"
            class="min-h-[4rem] w-full resize-y rounded-lg border border-input bg-background px-3 py-2.5 text-sm shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
            placeholder="Share something with the community…"
            :disabled="isPosting"
            aria-label="Feed post body"
          />
        </div>
        <div class="flex items-center justify-between gap-2 pl-[3.25rem]">
          <span
            class="type-caption"
            :class="composerOver ? 'text-destructive' : 'text-muted-foreground'"
          >
            {{ composerBody.length }} / {{ COMPOSER_MAX }}
          </span>
          <button
            type="submit"
            class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3.5 py-1.5 text-xs font-medium text-primary-foreground shadow-sm transition-opacity hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
            :disabled="!canSubmit || composerOver"
          >
            <Send class="h-3.5 w-3.5" aria-hidden="true" />
            {{ isPosting ? "Posting…" : "Post" }}
          </button>
        </div>
      </form>

      <div
        v-if="error"
        class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        Couldn't load the feed: {{ error }}
      </div>

      <div class="grid gap-3">
        <article
          v-for="entry in entries"
          :key="entry.id"
          class="group relative overflow-hidden rounded-xl border border-border bg-card shadow-sm transition-colors hover:bg-card/80"
        >
          <span
            class="absolute inset-y-0 left-0 w-1"
            :class="railClassFor(entry.source)"
            aria-hidden="true"
          ></span>
          <div class="grid gap-2 pl-4 pr-4 py-3.5">
            <header class="flex min-w-0 items-start gap-3">
              <AppAvatar :name="authorLabel(entry.author)" :src="null" size="md" />
              <div class="min-w-0 flex-1">
                <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <p class="type-control truncate font-semibold text-foreground">
                    {{ authorLabel(entry.author) }}
                  </p>
                  <span
                    v-if="entry.source"
                    class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[0.65rem] font-medium uppercase tracking-wide ring-1 ring-inset"
                    :class="chipClassFor(entry.source)"
                    :aria-label="labelFor(entry.source)"
                  >
                    <component
                      :is="iconFor(entry.source)"
                      class="h-3 w-3"
                      aria-hidden="true"
                    />
                    {{ labelFor(entry.source) }}
                  </span>
                </div>
                <p
                  v-if="entry.publishedMs"
                  class="type-caption text-muted-foreground"
                  :title="new Date(entry.publishedMs).toLocaleString()"
                >
                  {{ timeAgo(entry.publishedMs) }}
                </p>
              </div>
            </header>
            <h2
              v-if="entry.title"
              class="type-control font-semibold text-foreground"
            >
              {{ entry.title }}
            </h2>
            <p class="whitespace-pre-wrap break-words text-sm leading-relaxed text-foreground">
              {{ entry.body }}
            </p>
            <a
              v-if="entry.link"
              :href="entry.link"
              target="_blank"
              rel="noopener noreferrer"
              class="type-caption inline-flex items-center gap-1 text-primary underline-offset-2 hover:underline"
            >
              {{ entry.link }}
            </a>
          </div>
        </article>

        <template v-if="isLoading && entries.length === 0">
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
          v-else-if="entries.length === 0"
          class="flex flex-col items-center gap-2 rounded-xl border border-dashed border-border bg-card/40 px-6 py-10 text-center"
        >
          <span class="flex h-10 w-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
            <Inbox class="h-5 w-5" aria-hidden="true" />
          </span>
          <p class="type-control text-foreground">It's quiet in here</p>
          <p class="type-caption text-muted-foreground">
            {{ canPost ? "Share the first update — moods, activities, RSVPs and posts land here." : "Check back later." }}
          </p>
        </div>
      </div>
    </div>
  </div>
</template>

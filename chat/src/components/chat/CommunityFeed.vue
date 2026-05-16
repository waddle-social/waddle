<script setup lang="ts">
import { computed, ref } from "vue";
import { MessageSquareText, RefreshCw, Send } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import Skeleton from "@/components/ui/Skeleton.vue";
import type { FeedEntry, FeedPostInput } from "@/lib/xmpp-client";

interface CommunityFeedProps {
  entries: readonly FeedEntry[];
  isLoading: boolean;
  isPosting: boolean;
  error: string | null;
  canPost: boolean;
  selfJid: string | null;
}

const props = defineProps<CommunityFeedProps>();
const emit = defineEmits<{
  refresh: [];
  post: [input: FeedPostInput];
}>();

const composerBody = ref("");

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

function isOwnEntry(entry: FeedEntry): boolean {
  return !!props.selfJid && entry.author === props.selfJid.split("/")[0];
}

const canSubmit = computed(() => {
  return props.canPost && !props.isPosting && composerBody.value.trim().length > 0;
});

function submit() {
  const body = composerBody.value.trim();
  if (!body) return;
  emit("post", { body, ...(props.selfJid ? { author: props.selfJid.split("/")[0] } : {}) });
  composerBody.value = "";
}
</script>

<template>
  <section class="grid gap-3">
    <div class="flex items-center gap-2">
      <MessageSquareText class="h-4 w-4 text-primary" aria-hidden="true" />
      <h2 class="type-pane-title">Community Feed</h2>
      <button
        type="button"
        class="ml-auto inline-flex items-center gap-1 rounded-md border border-transparent px-2 py-1 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
        :disabled="isLoading"
        :aria-label="isLoading ? 'Refreshing' : 'Refresh feed'"
        @click="emit('refresh')"
      >
        <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': isLoading }" aria-hidden="true" />
        Refresh
      </button>
    </div>

    <!-- Composer -->
    <form
      v-if="canPost"
      class="grid gap-2 rounded-lg border border-border bg-card p-3"
      @submit.prevent="submit"
    >
      <textarea
        v-model="composerBody"
        class="min-h-[3.5rem] w-full resize-y rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        placeholder="Share something with the community…"
        :disabled="isPosting"
        aria-label="Feed post body"
      />
      <div class="flex items-center justify-between gap-2">
        <span class="type-caption text-muted-foreground">
          {{ composerBody.length }} chars
        </span>
        <button
          type="submit"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="!canSubmit"
        >
          <Send class="h-3.5 w-3.5" aria-hidden="true" />
          {{ isPosting ? "Posting…" : "Post" }}
        </button>
      </div>
    </form>

    <div v-if="error" class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
      Couldn't load the feed: {{ error }}
    </div>

    <!-- Entries -->
    <div class="grid gap-2">
      <article
        v-for="entry in entries"
        :key="entry.id"
        class="grid gap-2 rounded-lg border border-border bg-card px-4 py-3"
      >
        <header class="flex min-w-0 items-center gap-3">
          <AppAvatar :name="authorLabel(entry.author)" :src="null" size="sm" />
          <div class="min-w-0 flex-1">
            <p class="type-control truncate text-foreground">
              {{ authorLabel(entry.author) }}
              <span v-if="isOwnEntry(entry)" class="ml-1 text-xs text-muted-foreground">(you)</span>
            </p>
            <p
              v-if="entry.publishedMs"
              class="type-caption text-muted-foreground"
              :title="new Date(entry.publishedMs).toLocaleString()"
            >
              {{ timeAgo(entry.publishedMs) }}
            </p>
          </div>
        </header>
        <h3 v-if="entry.title" class="type-control font-semibold text-foreground">
          {{ entry.title }}
        </h3>
        <p class="whitespace-pre-wrap break-words text-sm text-foreground">
          {{ entry.body }}
        </p>
        <a
          v-if="entry.link"
          :href="entry.link"
          target="_blank"
          rel="noopener noreferrer"
          class="type-caption text-primary underline-offset-2 hover:underline"
        >{{ entry.link }}</a>
      </article>

      <template v-if="isLoading && entries.length === 0">
        <div
          v-for="i in 2"
          :key="`feed-skel-${i}`"
          class="grid gap-2 rounded-lg border border-border bg-card px-4 py-3"
          aria-hidden="true"
        >
          <div class="flex items-center gap-3">
            <Skeleton width="2rem" height="2rem" radius="9999px" />
            <div class="flex flex-1 flex-col gap-1.5">
              <Skeleton width="35%" height="0.7rem" />
              <Skeleton width="20%" height="0.55rem" />
            </div>
          </div>
          <Skeleton width="100%" height="0.75rem" />
          <Skeleton width="80%" height="0.75rem" />
        </div>
      </template>
      <div
        v-else-if="entries.length === 0"
        class="type-caption rounded-lg border border-border px-4 py-6 text-center text-muted-foreground"
      >
        No posts yet. {{ canPost ? "Be the first to share something." : "Check back later." }}
      </div>
    </div>
  </section>
</template>

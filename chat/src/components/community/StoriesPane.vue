<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Camera, ChevronLeft, ChevronRight, Plus, RefreshCw, X } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { Story, StoryPostInput } from "@/lib/xmpp-client";

interface StoriesPaneProps {
  stories: readonly Story[];
  isLoading: boolean;
  isPosting: boolean;
  error: string | null;
  canPost: boolean;
  selfJid: string | null;
}

const props = defineProps<StoriesPaneProps>();
const emit = defineEmits<{
  refresh: [];
  post: [input: StoryPostInput];
}>();

const composerOpen = ref(false);
const composerBody = ref("");
const composerMediaUrl = ref("");
const activeIndex = ref<number | null>(null);

function authorLabel(author: string | undefined): string {
  if (!author) return "Anonymous";
  return author.split("@")[0] ?? author;
}

function timeRemaining(story: Story, nowMs: number = Date.now()): string {
  if (typeof story.expiresMs !== "number") return "";
  const delta = story.expiresMs - nowMs;
  if (delta < 0) return "expired";
  if (delta < 60_000) return "<1m";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m`;
  return `${Math.floor(delta / 3_600_000)}h`;
}

const canSubmit = computed(() => {
  if (props.isPosting) return false;
  return composerBody.value.trim().length > 0 || composerMediaUrl.value.trim().length > 0;
});

const activeStory = computed<Story | null>(() => {
  if (activeIndex.value === null) return null;
  return props.stories[activeIndex.value] ?? null;
});

watch(
  () => props.stories.length,
  (length) => {
    if (activeIndex.value !== null && activeIndex.value >= length) {
      activeIndex.value = length > 0 ? 0 : null;
    }
  },
);

function selectStory(index: number) {
  activeIndex.value = index;
  composerOpen.value = false;
}

function closeReader() {
  activeIndex.value = null;
}

function prevStory() {
  if (activeIndex.value === null || activeIndex.value <= 0) return;
  activeIndex.value -= 1;
}

function nextStory() {
  if (activeIndex.value === null || activeIndex.value >= props.stories.length - 1) return;
  activeIndex.value += 1;
}

function submit() {
  const body = composerBody.value.trim();
  const mediaUrl = composerMediaUrl.value.trim();
  if (!body && !mediaUrl) return;
  emit("post", {
    ...(body ? { body } : {}),
    ...(mediaUrl ? { mediaUrl } : {}),
    ...(props.selfJid ? { author: props.selfJid.split("/")[0] } : {}),
  });
  composerBody.value = "";
  composerMediaUrl.value = "";
  composerOpen.value = false;
}

function dismissComposer() {
  composerOpen.value = false;
  composerBody.value = "";
  composerMediaUrl.value = "";
}
</script>

<template>
  <div class="chat-pane-scroll flex-1 min-h-0 bg-background px-[var(--chat-content-inline)] py-6">
    <div class="mx-auto grid w-full max-w-4xl gap-4">
      <header class="flex items-center gap-2">
        <Camera class="h-5 w-5 text-primary" aria-hidden="true" />
        <h1 class="type-pane-title text-foreground">Stories</h1>
        <span class="type-caption text-muted-foreground">{{ stories.length }} active</span>
        <button
          type="button"
          class="ml-auto inline-flex items-center gap-1 rounded-md border border-transparent px-2 py-1 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
          :disabled="isLoading"
          :aria-label="isLoading ? 'Refreshing' : 'Refresh stories'"
          @click="emit('refresh')"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': isLoading }" aria-hidden="true" />
          Refresh
        </button>
      </header>

      <div v-if="error" class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
        Couldn't post: {{ error }}
      </div>

      <!-- Horizontal rail -->
      <div class="flex gap-3 overflow-x-auto pb-2">
        <button
          v-if="canPost"
          type="button"
          class="flex w-[5.5rem] flex-shrink-0 flex-col items-center gap-2 rounded-lg border border-dashed border-border bg-card p-2 text-center text-muted-foreground hover:border-primary/60 hover:text-primary"
          :disabled="isPosting"
          @click="composerOpen = !composerOpen"
        >
          <span class="flex h-12 w-12 items-center justify-center rounded-full bg-muted">
            <Plus class="h-5 w-5" aria-hidden="true" />
          </span>
          <span class="type-caption truncate">{{ composerOpen ? "Close" : "Add" }}</span>
        </button>

        <button
          v-for="(story, index) in stories"
          :key="story.id"
          type="button"
          class="group flex w-[5.5rem] flex-shrink-0 flex-col items-center gap-2 rounded-lg p-2 hover:bg-muted/50"
          :class="activeIndex === index ? 'bg-muted/60' : ''"
          @click="selectStory(index)"
        >
          <span class="relative">
            <AppAvatar :name="authorLabel(story.author)" :src="story.mediaUrl ?? null" size="md" />
            <span class="pointer-events-none absolute inset-0 rounded-full ring-2 ring-primary/70 ring-offset-1 ring-offset-background" aria-hidden="true"></span>
          </span>
          <span class="min-w-0 type-caption text-foreground">
            <span class="block truncate">{{ authorLabel(story.author) }}</span>
            <span class="block text-muted-foreground">{{ timeRemaining(story) }}</span>
          </span>
        </button>

        <div
          v-if="stories.length === 0 && !canPost"
          class="type-caption flex-1 rounded-lg border border-border px-4 py-6 text-center text-muted-foreground"
        >
          No stories yet.
        </div>
      </div>

      <form
        v-if="composerOpen"
        class="grid gap-2 rounded-lg border border-border bg-card p-3"
        @submit.prevent="submit"
      >
        <textarea
          v-model="composerBody"
          class="min-h-[3rem] w-full resize-y rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          placeholder="Share a quick update…"
          :disabled="isPosting"
          aria-label="Story body"
        />
        <input
          v-model="composerMediaUrl"
          type="url"
          class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          placeholder="Optional image / video URL"
          :disabled="isPosting"
          aria-label="Story media URL"
        />
        <div class="flex items-center justify-between gap-2">
          <span class="type-caption text-muted-foreground">Expires in 24h</span>
          <div class="flex items-center gap-2">
            <button
              type="button"
              class="inline-flex items-center gap-1 rounded-md border border-input px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
              :disabled="isPosting"
              @click="dismissComposer"
            >
              <X class="h-3.5 w-3.5" aria-hidden="true" />
              Cancel
            </button>
            <button
              type="submit"
              class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
              :disabled="!canSubmit"
            >
              {{ isPosting ? "Posting…" : "Share" }}
            </button>
          </div>
        </div>
      </form>

      <!-- Reader -->
      <article
        v-if="activeStory"
        class="grid gap-3 rounded-xl border border-border bg-card p-4"
      >
        <header class="flex items-center gap-3">
          <AppAvatar :name="authorLabel(activeStory.author)" :src="null" size="sm" />
          <div class="min-w-0 flex-1">
            <p class="type-control truncate text-foreground">{{ authorLabel(activeStory.author) }}</p>
            <p v-if="activeStory.expiresMs" class="type-caption text-muted-foreground">
              {{ timeRemaining(activeStory) }} left
            </p>
          </div>
          <button
            type="button"
            class="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted/70 hover:text-foreground"
            aria-label="Close story"
            @click="closeReader"
          >
            <X class="h-4 w-4" aria-hidden="true" />
          </button>
        </header>

        <img
          v-if="activeStory.mediaUrl"
          :src="activeStory.mediaUrl"
          :alt="`Story media from ${authorLabel(activeStory.author)}`"
          class="max-h-[60vh] w-full rounded-md object-contain"
          referrerpolicy="no-referrer"
        />
        <p v-if="activeStory.body" class="whitespace-pre-wrap break-words text-sm text-foreground">
          {{ activeStory.body }}
        </p>

        <div class="flex items-center justify-between gap-2 border-t border-border pt-3">
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded-md border border-input px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            :disabled="activeIndex === null || activeIndex <= 0"
            @click="prevStory"
          >
            <ChevronLeft class="h-3.5 w-3.5" aria-hidden="true" />
            Prev
          </button>
          <span class="type-caption text-muted-foreground" v-if="activeIndex !== null">
            {{ activeIndex + 1 }} / {{ stories.length }}
          </span>
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded-md border border-input px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            :disabled="activeIndex === null || activeIndex >= stories.length - 1"
            @click="nextStory"
          >
            Next
            <ChevronRight class="h-3.5 w-3.5" aria-hidden="true" />
          </button>
        </div>
      </article>
    </div>
  </div>
</template>

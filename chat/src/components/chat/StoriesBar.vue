<script setup lang="ts">
import { computed, ref } from "vue";
import { Camera, Plus, X } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { Story, StoryPostInput } from "@/lib/xmpp-client";

interface StoriesBarProps {
  stories: readonly Story[];
  isLoading: boolean;
  isPosting: boolean;
  error: string | null;
  canPost: boolean;
  selfJid: string | null;
}

const props = defineProps<StoriesBarProps>();
const emit = defineEmits<{
  post: [input: StoryPostInput];
  view: [story: Story];
}>();

const composerOpen = ref(false);
const composerBody = ref("");
const composerMediaUrl = ref("");

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
  <section
    v-if="canPost || stories.length > 0"
    class="grid gap-3"
    aria-label="Stories"
  >
    <div class="flex items-center gap-2">
      <Camera class="h-4 w-4 text-primary" aria-hidden="true" />
      <h2 class="type-pane-title">Stories</h2>
      <span class="ml-auto type-caption text-muted-foreground">
        {{ stories.length }} {{ stories.length === 1 ? "active" : "active" }}
      </span>
    </div>

    <div v-if="error" class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
      Couldn't post: {{ error }}
    </div>

    <!-- Horizontal scroll strip: composer pill + story bubbles -->
    <div class="flex gap-3 overflow-x-auto pb-2">
      <button
        v-if="canPost"
        type="button"
        class="flex w-[5rem] flex-shrink-0 flex-col items-center gap-2 rounded-lg border border-dashed border-border bg-card p-2 text-center text-muted-foreground hover:border-primary/60 hover:text-primary"
        :disabled="isPosting"
        :aria-label="composerOpen ? 'Close story composer' : 'Add a story'"
        @click="composerOpen = !composerOpen"
      >
        <span class="flex h-12 w-12 items-center justify-center rounded-full bg-muted">
          <Plus class="h-5 w-5" aria-hidden="true" />
        </span>
        <span class="type-caption truncate text-current">
          {{ composerOpen ? "Close" : "Add" }}
        </span>
      </button>

      <button
        v-for="story in stories"
        :key="story.id"
        type="button"
        class="group flex w-[5rem] flex-shrink-0 flex-col items-center gap-2 rounded-lg p-2 hover:bg-muted/50"
        :aria-label="`View story by ${authorLabel(story.author)}`"
        @click="emit('view', story)"
      >
        <span class="relative">
          <AppAvatar :name="authorLabel(story.author)" :src="story.mediaUrl ?? null" size="md" />
          <span
            class="pointer-events-none absolute inset-0 rounded-full ring-2 ring-primary/70 ring-offset-1 ring-offset-background"
            aria-hidden="true"
          ></span>
        </span>
        <span class="min-w-0 type-caption text-foreground">
          <span class="block truncate">{{ authorLabel(story.author) }}</span>
          <span class="block text-muted-foreground">{{ timeRemaining(story) }}</span>
        </span>
      </button>

      <template v-if="isLoading && stories.length === 0 && !canPost">
        <div
          v-for="i in 3"
          :key="`story-skel-${i}`"
          class="flex w-[5rem] flex-shrink-0 flex-col items-center gap-2 rounded-lg p-2"
          aria-hidden="true"
        >
          <span class="h-12 w-12 animate-pulse rounded-full bg-muted"></span>
          <span class="h-3 w-3/4 animate-pulse rounded bg-muted"></span>
        </div>
      </template>
    </div>

    <!-- Composer panel slides under the strip when open -->
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
        <span class="type-caption text-muted-foreground">
          Expires in 24h
        </span>
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
  </section>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { ChevronLeft, ChevronRight, X } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { QUICK_REACTION_EMOJIS } from "@/lib/reaction-mode";
import type { Story, StoryReactionSummary } from "@/lib/xmpp-client";

interface StoryReactionEntry {
  emoji: string;
  count: number;
  reactors: readonly string[];
}

const props = defineProps<{
  story: Story | null;
  storyIndex: number | null;
  storyCount: number;
  reactionSummary: StoryReactionSummary | null;
  reactionEntries: readonly StoryReactionEntry[];
}>();

const emit = defineEmits<{
  close: [];
  previous: [];
  next: [];
  react: [emoji: string];
}>();

const dialogEl = ref<HTMLElement | null>(null);
const previouslyFocused = ref<HTMLElement | null>(null);

const hasPrevious = computed(() => props.storyIndex !== null && props.storyIndex > 0);
const hasNext = computed(() => props.storyIndex !== null && props.storyIndex < props.storyCount - 1);

watch(
  () => props.story,
  async (story, previous) => {
    if (story && !previous) {
      previouslyFocused.value = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      await nextTick();
      dialogEl.value?.focus();
    } else if (!story && previous) {
      previouslyFocused.value?.focus();
      previouslyFocused.value = null;
    }
  },
);

function listedAgo(story: Story): string {
  if (typeof story.postedMs !== "number") return "";
  const delta = Math.max(0, Date.now() - story.postedMs);
  if (delta < 60_000) return "just now";
  if (delta < 3_600_000) return `${plural(Math.floor(delta / 60_000), "minute")} ago`;
  if (delta < 86_400_000) return `${plural(Math.floor(delta / 3_600_000), "hour")} ago`;
  if (delta < 604_800_000) return `${plural(Math.floor(delta / 86_400_000), "day")} ago`;
  return new Date(story.postedMs).toLocaleDateString();
}

function plural(value: number, unit: string): string {
  return `${value} ${unit}${value === 1 ? "" : "s"}`;
}

function authorLabel(author: string | undefined): string {
  if (!author) return "Anonymous";
  return author.split("@")[0] ?? author;
}

function isVideoStory(story: Story): boolean {
  return Boolean(story.mediaUrl && /\.(mp4|webm|mov)(\?|$)/i.test(story.mediaUrl));
}

function eventTargetUsesArrows(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && Boolean(target.closest("video,audio,input,textarea,select,[contenteditable='true']"));
}

function focusableElements(): HTMLElement[] {
  const root = dialogEl.value;
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(
    "button:not([disabled]), a[href], input:not([disabled]), textarea:not([disabled]), select:not([disabled]), video[controls], [tabindex]:not([tabindex='-1'])",
  )).filter((el) => !el.hasAttribute("disabled") && el.tabIndex !== -1);
}

function trapTab(event: KeyboardEvent) {
  const focusables = focusableElements();
  if (focusables.length === 0) {
    event.preventDefault();
    dialogEl.value?.focus();
    return;
  }
  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last?.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first?.focus();
  }
}

function onKeydown(event: KeyboardEvent) {
  if (!props.story) return;
  if (event.key === "Escape") {
    event.preventDefault();
    emit("close");
    return;
  }
  if (event.key === "Tab") {
    trapTab(event);
    return;
  }
  if (eventTargetUsesArrows(event.target)) return;
  if (event.key === "ArrowLeft" && hasPrevious.value) {
    event.preventDefault();
    emit("previous");
  } else if (event.key === "ArrowRight" && hasNext.value) {
    event.preventDefault();
    emit("next");
  }
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="story"
      class="z-modal fixed inset-0 flex items-end justify-center p-3 sm:items-center sm:p-6"
      role="presentation"
    >
      <div class="absolute inset-0 bg-background/75 backdrop-blur-md" @click="emit('close')" />
      <article
        ref="dialogEl"
        class="relative grid max-h-[calc(100dvh-1.5rem)] w-full max-w-2xl grid-rows-[auto_minmax(0,1fr)_auto_auto] overflow-hidden rounded-lg border border-border bg-card shadow-2xl animate-slide-up"
        role="dialog"
        aria-modal="true"
        aria-label="Story"
        tabindex="-1"
        @click.stop
        @keydown="onKeydown"
      >
        <header class="flex items-center gap-3 border-b border-border px-4 py-3">
          <AppAvatar :name="authorLabel(story.author)" :src="null" size="sm" />
          <div class="min-w-0 flex-1">
            <p class="type-control truncate text-foreground">{{ authorLabel(story.author) }}</p>
            <p v-if="listedAgo(story)" class="type-caption text-muted-foreground">
              {{ listedAgo(story) }}
            </p>
          </div>
          <button
            type="button"
            class="inline-flex h-8 w-8 items-center justify-center rounded-md text-muted-foreground hover:bg-muted/70 hover:text-foreground"
            aria-label="Close story"
            @click="emit('close')"
          >
            <X class="h-4 w-4" aria-hidden="true" />
          </button>
        </header>

        <div class="flex min-h-0 flex-col gap-3 overflow-y-auto px-4 py-3">
          <div v-if="story.mediaUrl" class="flex min-h-[12rem] items-center justify-center rounded-md bg-muted">
            <video
              v-if="isVideoStory(story)"
              :src="story.mediaUrl"
              class="max-h-[min(56dvh,34rem)] w-full rounded-md"
              controls
              playsinline
              referrerpolicy="no-referrer"
            ></video>
            <img
              v-else
              :src="story.mediaUrl"
              :alt="`Story media from ${authorLabel(story.author)}`"
              class="max-h-[min(56dvh,34rem)] w-full rounded-md object-contain"
              referrerpolicy="no-referrer"
            />
          </div>
          <p v-if="story.body" class="whitespace-pre-wrap break-words text-sm text-foreground">
            {{ story.body }}
          </p>
        </div>

        <div class="grid gap-2 border-t border-border px-4 py-3">
          <div class="flex flex-wrap items-center gap-1.5">
            <button
              v-for="emoji in QUICK_REACTION_EMOJIS"
              :key="emoji"
              type="button"
              class="inline-flex h-8 min-w-8 items-center justify-center rounded-md border px-2 text-sm hover:bg-muted/60"
              :class="reactionSummary?.mine.includes(emoji) ? 'border-primary bg-primary/10 text-primary' : 'border-input text-foreground'"
              :aria-label="`React with ${emoji}`"
              :aria-pressed="reactionSummary?.mine.includes(emoji) ? 'true' : 'false'"
              @click="emit('react', emoji)"
            >
              {{ emoji }}
            </button>
          </div>
          <div v-if="reactionEntries.length > 0" class="flex flex-wrap gap-1.5">
            <span
              v-for="entry in reactionEntries"
              :key="entry.emoji"
              class="inline-flex items-center gap-1 rounded-md border border-border bg-muted/40 px-2 py-1 text-xs text-foreground"
              :title="entry.reactors.join(', ')"
            >
              <span>{{ entry.emoji }}</span>
              <span class="text-muted-foreground">{{ entry.count }}</span>
            </span>
          </div>
        </div>

        <div class="flex items-center justify-between gap-2 border-t border-border px-4 py-3">
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded-md border border-input px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            :disabled="!hasPrevious"
            @click="emit('previous')"
          >
            <ChevronLeft class="h-3.5 w-3.5" aria-hidden="true" />
            Prev
          </button>
          <span class="type-caption text-muted-foreground" v-if="storyIndex !== null">
            {{ storyIndex + 1 }} / {{ storyCount }}
          </span>
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded-md border border-input px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            :disabled="!hasNext"
            @click="emit('next')"
          >
            Next
            <ChevronRight class="h-3.5 w-3.5" aria-hidden="true" />
          </button>
        </div>
      </article>
    </div>
  </Teleport>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { X } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { Story } from "@/lib/xmpp-client";

interface StoryViewerProps {
  story: Story;
}

const props = defineProps<StoryViewerProps>();
const emit = defineEmits<{ close: [] }>();

function authorLabel(author: string | undefined): string {
  if (!author) return "Anonymous";
  return author.split("@")[0] ?? author;
}

const remaining = computed(() => {
  if (typeof props.story.expiresMs !== "number") return null;
  const delta = props.story.expiresMs - Date.now();
  if (delta < 0) return "expired";
  if (delta < 60_000) return "<1m left";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m left`;
  return `${Math.floor(delta / 3_600_000)}h left`;
});

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") emit("close");
}
</script>

<template>
  <Teleport to="body">
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-background/85 backdrop-blur-sm"
      role="dialog"
      aria-modal="true"
      :aria-label="`Story by ${authorLabel(story.author)}`"
      tabindex="-1"
      @keydown="onKeydown"
      @click.self="emit('close')"
    >
      <div class="relative grid w-full max-w-md gap-3 rounded-xl border border-border bg-card p-4 shadow-xl">
        <button
          type="button"
          class="absolute right-3 top-3 inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted/70 hover:text-foreground"
          aria-label="Close story"
          @click="emit('close')"
        >
          <X class="h-4 w-4" aria-hidden="true" />
        </button>

        <header class="flex items-center gap-3">
          <AppAvatar :name="authorLabel(story.author)" :src="null" size="sm" />
          <div class="min-w-0 flex-1">
            <p class="type-control truncate text-foreground">{{ authorLabel(story.author) }}</p>
            <p v-if="remaining" class="type-caption text-muted-foreground">{{ remaining }}</p>
          </div>
        </header>

        <img
          v-if="story.mediaUrl"
          :src="story.mediaUrl"
          :alt="`Story media from ${authorLabel(story.author)}`"
          class="max-h-[60vh] w-full rounded-md object-contain"
          referrerpolicy="no-referrer"
        />
        <p v-if="story.body" class="whitespace-pre-wrap break-words text-sm text-foreground">
          {{ story.body }}
        </p>
      </div>
    </div>
  </Teleport>
</template>

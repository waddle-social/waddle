<script setup lang="ts">
import { computed } from "vue";
import { MessageSquare, Phone, Video } from "lucide-vue-next";
import type { CallAnchorCardState } from "@/lib/call-thread-anchor";

const props = defineProps<{
  state: CallAnchorCardState;
}>();

const emit = defineEmits<{
  join: [];
  openThread: [threadId: string];
}>();

const isLive = computed(() => props.state.status === "live");
const MediaIcon = computed(() => props.state.media.video ? Video : Phone);
const participantSummary = computed(() => {
  if (props.state.participantLabels.length === 0) return "No one connected";
  const visible = props.state.participantLabels.slice(0, 3).join(", ");
  const extra = props.state.participantLabels.length - 3;
  return extra > 0 ? `${visible} +${extra}` : visible;
});
const messageCountLabel = computed(() =>
  `${props.state.messageCount} ${props.state.messageCount === 1 ? "message" : "messages"} in call chat`
);
const showAction = computed(() => props.state.actionLabel !== null);

function joinCall() {
  if (!isLive.value || props.state.actionDisabled) return;
  emit("join");
}

function openThread() {
  if (!props.state.threadId) return;
  emit("openThread", props.state.threadId);
}
</script>

<template>
  <section
    class="call-anchor-card rounded-lg border border-border bg-card px-3 py-2.5 shadow-sm"
    :class="{
      'call-anchor-card--live border-primary/35 bg-primary/5': isLive,
      'call-anchor-card--ended opacity-70': !isLive,
    }"
    :aria-label="state.ariaLabel"
  >
    <div class="flex min-w-0 items-center gap-3">
      <div class="relative flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-full border border-border bg-background text-muted-foreground">
        <span
          v-if="isLive"
          class="call-anchor-card__pulse absolute -right-0.5 -top-0.5 h-2.5 w-2.5 rounded-full bg-success motion-safe:animate-pulse"
          aria-hidden="true"
        />
        <component :is="MediaIcon" class="h-4 w-4" aria-hidden="true" />
      </div>
      <div class="min-w-0 flex-1">
        <div class="flex min-w-0 items-center gap-2">
          <p class="type-field-sm truncate text-foreground">{{ state.title }}</p>
          <span class="type-meta type-numeric text-muted-foreground">
            {{ state.participantCount }}
          </span>
        </div>
        <p class="type-caption truncate text-muted-foreground">{{ participantSummary }}</p>
      </div>
      <button
        v-if="showAction"
        type="button"
        class="inline-flex h-8 flex-shrink-0 items-center gap-1.5 rounded-md bg-primary px-3 type-control text-primary-foreground hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        :class="{ 'cursor-not-allowed opacity-60': state.actionDisabled }"
        :aria-label="state.ariaLabel"
        :disabled="state.actionDisabled"
        @click.stop="joinCall"
      >
        <component :is="MediaIcon" class="h-3.5 w-3.5" aria-hidden="true" />
        <span>{{ state.actionLabel }}</span>
      </button>
    </div>
    <button
      v-if="state.threadId"
      type="button"
      class="mt-2 inline-flex max-w-full items-center gap-1.5 type-caption text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      @click.stop="openThread"
    >
      <MessageSquare class="h-3.5 w-3.5 flex-shrink-0" aria-hidden="true" />
      <span class="truncate">{{ messageCountLabel }} ›</span>
    </button>
  </section>
</template>

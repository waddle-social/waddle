<script setup lang="ts">
import { computed, nextTick, ref, watch, watchEffect } from "vue";
import { useVirtualizer } from "@tanstack/vue-virtual";
import type { TimelineMessage } from "@/lib/chat-ui";

const props = withDefaults(defineProps<{
  items: TimelineMessage[];
  hasOlder: boolean;
  loadingOlder: boolean;
  sentinelPosition: "start" | "end";
  ariaLabel: string;
  contentClass?: string;
}>(), {
  contentClass: "chat-message-lane chat-message-list",
});

const emit = defineEmits<{
  loadOlder: [];
  scroll: [event: Event];
}>();

const scrollElement = ref<HTMLDivElement | null>(null);
const hasOlderSentinel = computed(() => props.hasOlder || props.loadingOlder);
const rowCount = computed(() => props.items.length + (hasOlderSentinel.value ? 1 : 0));
const sentinelIndex = computed(() =>
  !hasOlderSentinel.value ? -1 : props.sentinelPosition === "start" ? 0 : rowCount.value - 1,
);

function itemForVirtualIndex(index: number): TimelineMessage | null {
  if (index === sentinelIndex.value) return null;
  const offset = hasOlderSentinel.value && props.sentinelPosition === "start" ? 1 : 0;
  return props.items[index - offset] ?? null;
}

const virtualizer = useVirtualizer(computed(() => ({
  count: rowCount.value,
  getScrollElement: () => scrollElement.value,
  estimateSize: (index: number) => (index === sentinelIndex.value ? 44 : 112),
  overscan: 8,
  getItemKey: (index: number) => itemForVirtualIndex(index)?.id ?? "older-history-sentinel",
})));

const virtualItems = computed(() => virtualizer.value.getVirtualItems());
const totalSize = computed(() => virtualizer.value.getTotalSize());

function maybeLoadOlder() {
  if (!props.hasOlder || props.loadingOlder || sentinelIndex.value === -1) return;
  const visible = virtualItems.value.some((item) => item.index === sentinelIndex.value);
  if (visible) emit("loadOlder");
}

watchEffect(maybeLoadOlder);

watch(
  () => props.items.length,
  () => {
    void nextTick(() => virtualizer.value.measure());
  },
);

async function scrollToMessageId(messageId: string, align: "start" | "center" | "end" = "center") {
  const index = props.items.findIndex((item) => item.id === messageId);
  if (index === -1) return false;
  const offset = hasOlderSentinel.value && props.sentinelPosition === "start" ? 1 : 0;
  virtualizer.value.scrollToIndex(index + offset, { align });
  await nextTick();
  return true;
}

defineExpose({ scrollElement, scrollToMessageId });
</script>

<template>
  <div
    ref="scrollElement"
    class="chat-pane-scroll chat-message-scroll flex-1 min-h-0 overflow-auto px-[var(--chat-content-inline)]"
    :aria-label="ariaLabel"
    @scroll="(event) => { emit('scroll', event); maybeLoadOlder(); }"
  >
    <div :class="contentClass" :style="{ height: `${totalSize}px`, position: 'relative' }">
      <div
        v-for="virtualRow in virtualItems"
        :key="virtualRow.key"
        :data-index="virtualRow.index"
        :ref="virtualizer.measureElement"
        class="absolute left-0 top-0 w-full"
        :style="{ transform: `translateY(${virtualRow.start}px)` }"
      >
        <slot
          v-if="itemForVirtualIndex(virtualRow.index)"
          name="item"
          :item="itemForVirtualIndex(virtualRow.index)"
        />
        <div
          v-else
          class="type-caption flex items-center justify-center py-3 text-muted-foreground"
          data-older-history-sentinel
        >
          <button
            v-if="hasOlder && !loadingOlder"
            type="button"
            class="rounded-full border border-border bg-background px-3 py-1.5 transition-colors hover:bg-muted"
            @click="emit('loadOlder')"
          >
            Load older messages
          </button>
          <span v-else>Loading older messages…</span>
        </div>
      </div>
    </div>
  </div>
</template>

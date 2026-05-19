<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, watchEffect } from "vue";
import { useVirtualizer } from "@tanstack/vue-virtual";
import type { TimelineMessage } from "@/lib/chat-ui";
import {
  isTopPinnedScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import { installVirtualTimelineMeasurementRecovery } from "@/ui/virtual-timeline-measurement";

const props = withDefaults(defineProps<{
  items: TimelineMessage[];
  hasOlder: boolean;
  loadingOlder: boolean;
  // "start" / "end" anchor the older-history sentinel to the edge of the
  // virtualized list. "before-end" parks it one slot in from the end so the
  // sentinel can sit between the loaded items and a non-paginated trailing
  // anchor (e.g. the thread root rendered at the chronologically-oldest end
  // in social mode).
  sentinelPosition: "start" | "end" | "before-end";
  ariaLabel: string;
  contentClass?: string;
}>(), {
  contentClass: "chat-message-list",
});

const emit = defineEmits<{
  loadOlder: [];
  scroll: [event: Event];
}>();

const scrollElement = ref<HTMLDivElement | null>(null);
const hasOlderSentinel = computed(() => props.hasOlder || props.loadingOlder);
const rowCount = computed(() => props.items.length + (hasOlderSentinel.value ? 1 : 0));
const sentinelIndex = computed(() => {
  if (!hasOlderSentinel.value) return -1;
  if (props.sentinelPosition === "start") return 0;
  if (props.sentinelPosition === "before-end") return Math.max(0, rowCount.value - 2);
  return rowCount.value - 1;
});

function itemIndexForVirtualIndex(index: number): number {
  if (!hasOlderSentinel.value) return index;
  if (props.sentinelPosition === "start") return index - 1;
  if (props.sentinelPosition === "before-end") return index < sentinelIndex.value ? index : index - 1;
  return index;
}

function virtualIndexForItemIndex(itemIndex: number): number {
  if (!hasOlderSentinel.value) return itemIndex;
  if (props.sentinelPosition === "start") return itemIndex + 1;
  if (props.sentinelPosition === "before-end") return itemIndex < sentinelIndex.value ? itemIndex : itemIndex + 1;
  return itemIndex;
}

function itemForVirtualIndex(index: number): TimelineMessage | null {
  if (index === sentinelIndex.value) return null;
  return props.items[itemIndexForVirtualIndex(index)] ?? null;
}

const virtualizer = useVirtualizer(computed(() => ({
  count: rowCount.value,
  getScrollElement: () => scrollElement.value,
  estimateSize: (index: number) => (index === sentinelIndex.value ? 44 : 112),
  overscan: 8,
  getItemKey: (index: number) => itemForVirtualIndex(index)?.id ?? "older-history-sentinel",
})));

let disconnectMeasurementRecovery: (() => void) | null = null;

watch(
  scrollElement,
  (el) => {
    disconnectMeasurementRecovery?.();
    disconnectMeasurementRecovery = null;
    if (!el) return;
    disconnectMeasurementRecovery = installVirtualTimelineMeasurementRecovery({
      scrollElement: el,
      measure: () => virtualizer.value.measure(),
      measureElement: (row) => virtualizer.value.measureElement(row),
    });
  },
  { flush: "post", immediate: true },
);

onBeforeUnmount(() => {
  disconnectMeasurementRecovery?.();
  disconnectMeasurementRecovery = null;
});

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
  virtualizer.value.scrollToIndex(virtualIndexForItemIndex(index), { align });
  await nextTick();
  return true;
}

async function scrollToPinnedEdge(mode: ScrollDirectionMode) {
  if (props.items.length === 0) return false;
  const itemIndex = isTopPinnedScrollDirection(mode) ? 0 : props.items.length - 1;
  virtualizer.value.scrollToIndex(virtualIndexForItemIndex(itemIndex), {
    align: isTopPinnedScrollDirection(mode) ? "start" : "end",
  });
  await nextTick();
  // The virtualizer's `scrollToIndex` plans the scroll using the
  // `estimateSize` heights (112px / 44px); real rows — especially
  // ones with images, code blocks, or extension cards — measure much
  // taller. After `scrollToIndex` we're "close to" the edge, but the
  // measurement reconciliation that follows grows scrollHeight, so we
  // end up several rows shy of the actual last message.
  //
  // Pin scrollTop directly across a short raf loop, bailing once the
  // scrollHeight stops changing for two consecutive frames. The
  // browser clamps to (scrollHeight - clientHeight) for us so this
  // always lands at the true edge once measurements settle. Capped
  // at 8 frames so a runaway re-render can't spin the loop.
  const el = scrollElement.value;
  if (!el) return true;
  await new Promise<void>((resolve) => {
    const topPinned = isTopPinnedScrollDirection(mode);
    let lastHeight = -1;
    let stable = 0;
    let frames = 0;
    function tick() {
      el!.scrollTop = topPinned ? 0 : el!.scrollHeight;
      if (el!.scrollHeight === lastHeight) {
        if (++stable >= 2) return resolve();
      } else {
        stable = 0;
        lastHeight = el!.scrollHeight;
      }
      if (++frames >= 8) return resolve();
      requestAnimationFrame(tick);
    }
    requestAnimationFrame(tick);
  });
  return true;
}

defineExpose({ scrollElement, scrollToMessageId, scrollToPinnedEdge });
</script>

<template>
  <div
    ref="scrollElement"
    class="chat-pane-scroll chat-message-scroll flex-1 min-h-0 px-[var(--chat-content-inline)]"
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

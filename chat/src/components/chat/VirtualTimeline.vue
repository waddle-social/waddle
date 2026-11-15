<script setup lang="ts">
import { resolveRoomMessageTarget } from "@/lib/messaging/room-message-target";
import { timelineRowKey } from "@/lib/timeline-row-key";
import { computed, nextTick, onBeforeUnmount, ref, watch, watchEffect } from "vue";
import { useVirtualizer } from "@tanstack/vue-virtual";
import { css, cx } from "styled-system/css";
import { btn } from "styled-system/recipes";
import type { TimelineMessage } from "@/lib/chat-ui";
import {
  isTopPinnedScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import {
  adjustScrollForRowsAboveViewport,
  installVirtualTimelineMeasurementRecovery,
} from "@/ui/virtual-timeline-measurement";

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
  contentClass: "",
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
  getItemKey: (index: number) => {
    const message = itemForVirtualIndex(index);
    return message ? timelineRowKey(message) : "older-history-sentinel";
  },
})));
virtualizer.value.shouldAdjustScrollPositionOnItemSizeChange = adjustScrollForRowsAboveViewport;

let disconnectMeasurementRecovery: (() => void) | null = null;

watch(
  scrollElement,
  (el) => {
    disconnectMeasurementRecovery?.();
    disconnectMeasurementRecovery = null;
    if (!el) return;
    disconnectMeasurementRecovery = installVirtualTimelineMeasurementRecovery({
      scrollElement: el,
      // Re-measure the mounted rows in place. `virtualizer.measure()` clears
      // the whole size cache, so every row snaps to the 112px estimate and
      // the timeline visibly jumps on tab refocus before re-measuring.
      measure: () => {
        for (const row of el.firstElementChild?.children ?? []) {
          virtualizer.value.measureElement(row as HTMLElement);
        }
      },
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

async function scrollToMessageId(messageId: string, align: "start" | "center" | "end" = "center") {
  const rowKeyMatches = props.items.filter((item) => item.rowKey === messageId);
  if (rowKeyMatches.length > 1) return false;
  let target = rowKeyMatches[0];
  if (rowKeyMatches.length === 0) {
    if (props.items.some((item) => !!item.authorOccupantJid)) {
      target = resolveRoomMessageTarget(props.items, messageId).message;
    } else {
      target = props.items.find((item) => item.id === messageId);
    }
  }
  const index = target ? props.items.indexOf(target) : -1;
  if (index === -1) return false;
  virtualizer.value.scrollToIndex(virtualIndexForItemIndex(index), { align });
  await nextTick();
  return true;
}

// Rows keep growing after the pin settles (late measurement, images, link
// previews), which pushes the newest text below the fold. While the reader
// is parked at the bottom edge, follow every growth of the list.
const STICK_TOLERANCE_PX = 64;
let followBottom = false;
let bottomMode = false;
const listElement = ref<HTMLDivElement | null>(null);

function distanceFromBottom(el: HTMLElement): number {
  return el.scrollHeight - el.scrollTop - el.clientHeight;
}

function onScroll(event: Event) {
  const el = scrollElement.value;
  if (el && bottomMode) followBottom = distanceFromBottom(el) <= STICK_TOLERANCE_PX;
  emit("scroll", event);
  maybeLoadOlder();
}

let followObserver: ResizeObserver | null = null;
watch(listElement, (list) => {
  followObserver?.disconnect();
  followObserver = null;
  if (!list || typeof ResizeObserver === "undefined") return;
  followObserver = new ResizeObserver(() => {
    const el = scrollElement.value;
    if (el && followBottom && distanceFromBottom(el) > 0) el.scrollTop = el.scrollHeight;
  });
  followObserver.observe(list);
  if (scrollElement.value) followObserver.observe(scrollElement.value);
}, { flush: "post" });
onBeforeUnmount(() => followObserver?.disconnect());

async function scrollToPinnedEdge(mode: ScrollDirectionMode) {
  if (props.items.length === 0) return false;
  bottomMode = !isTopPinnedScrollDirection(mode);
  followBottom = bottomMode;
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

// Chat panes never scroll sideways: long text wraps, <pre> scrolls itself.
const scrollClass = css({
  display: "flex",
  flexDirection: "column",
  flex: "1",
  minHeight: 0,
  overflowX: "hidden",
  overflowY: "auto",
  scrollbarGutter: "stable",
  px: "2",
  py: "4",
});
const listClass = css({ position: "relative", flex: "none" });
const rowClass = css({ position: "absolute", left: 0, top: 0, width: "100%" });
const sentinelClass = css({ display: "flex", alignItems: "center", justifyContent: "center", py: "3", textStyle: "caption", color: "fg.muted" });
</script>

<template>
  <div
    ref="scrollElement"
    :class="scrollClass"
    :aria-label="ariaLabel"
    @scroll="onScroll"
  >
    <div ref="listElement" :class="cx(listClass, contentClass)" :style="{ height: `${totalSize}px` }">
      <div
        v-for="virtualRow in virtualItems"
        :key="virtualRow.key"
        :data-index="virtualRow.index"
        :ref="virtualizer.measureElement"
        :class="rowClass"
        :style="{ transform: `translateY(${virtualRow.start}px)` }"
      >
        <slot
          v-if="itemForVirtualIndex(virtualRow.index)"
          name="item"
          :item="itemForVirtualIndex(virtualRow.index)"
        />
        <div
          v-else
          :class="sentinelClass"
          data-older-history-sentinel
        >
          <button
            v-if="hasOlder && !loadingOlder"
            type="button"
            :class="btn({ size: 'sm' })"
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

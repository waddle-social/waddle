<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import type { LocalMediaTrack, RemoteMediaTrack } from "@/lib/calls/engine";
import { TileAttachments, type TileAttachable } from "@/lib/calls/tile-attach";
import { selectGridLayout } from "@/lib/calls/grid-layout";
import { buildCallTiles, type CallTileModel } from "@/lib/calls/call-tiles";
import CallTile from "./CallTile.vue";

/**
 * Participant tile grid for the in-call surfaces (split + expanded).
 *
 * The layout follows LiveKit's `GridLayout` reference: a small lookup
 * table of `(cols, rows)` definitions in `lib/calls/grid-layout.ts`
 * picks the smallest layout whose capacity covers the participant
 * count and whose `min{Width,Height}` floors fit the container.
 *
 * - Grid CSS: `grid-template-columns: repeat(--cols, minmax(0, 1fr))`
 *   plus `grid-auto-rows: minmax(0, 1fr)`. The `minmax(0, 1fr)` is
 *   critical — without the `0` floor, the tracks won't shrink below
 *   the intrinsic content size and the grid overflows during a
 *   splitter drag.
 * - Cells fill the container fully; the tile inside renders its
 *   video as `object-fit: cover` and overlays a gradient placeholder
 *   for the no-video state. Per-tile aspect-ratio is deliberately
 *   absent so a vertical-drag of the splitter reflows tile shape
 *   instead of forcing scroll.
 * - A `ResizeObserver` on the grid container re-runs the layout pick
 *   on every dimension change (splitter drag, app-shell resize). Tile
 *   count changes trigger the same recompute via Vue reactivity.
 *
 * Speaker focus (one big tile + horizontal thumbnail strip) is kept
 * as a separate branch inside this template — same algorithm doesn't
 * apply, since the layout is "1 big + N small", not "N equal".
 */
type Tile = CallTileModel;

const props = defineProps<{
  remoteTracks: RemoteMediaTrack[];
  localTracks: LocalMediaTrack[];
  /** LiveKit identity for the local participant (may be null pre-connect). */
  localIdentity: string | null;
  /** Whether the local mic is currently un-muted. */
  micEnabled: boolean;
}>();

const tiles = computed<Tile[]>(() => {
  return buildCallTiles({
    remoteTracks: props.remoteTracks,
    localTracks: props.localTracks,
    localIdentity: props.localIdentity,
    micEnabled: props.micEnabled,
  });
});

const focusedKey = ref<string | null>(null);
const focusedTile = computed<Tile | null>(() => {
  if (!focusedKey.value) return null;
  return tiles.value.find((t) => t.key === focusedKey.value) ?? null;
});
const otherTiles = computed<Tile[]>(() => {
  if (!focusedTile.value) return [];
  return tiles.value.filter((t) => t.key !== focusedTile.value?.key);
});

function toggleFocus(tile: Tile): void {
  focusedKey.value = focusedKey.value === tile.key ? null : tile.key;
}

// One `TileAttachments` per grid instance — reconciles (element,
// track) pairs so LiveKit's `attachedElements` never accumulates
// stale entries across focus toggles or participant churn.
const attachments = new TileAttachments();
function attach(
  key: string,
  el: HTMLMediaElement | null,
  track: TileAttachable | null,
): void {
  attachments.sync(key, el, track);
}

// Container size driven by `ResizeObserver` so splitter drags and
// app-shell width changes both reflow the grid. We seed with a
// synchronous `getBoundingClientRect` so the very first paint isn't
// the 1×1 fallback.
const gridEl = ref<HTMLElement | null>(null);
const containerSize = ref<{ width: number; height: number }>({ width: 0, height: 0 });

function setGridEl(el: Element | null): void {
  gridEl.value = el instanceof HTMLElement ? el : null;
}

let observer: ResizeObserver | null = null;
watch(gridEl, (el, _prev, onCleanup) => {
  if (!el || typeof ResizeObserver === "undefined") return;
  const rect = el.getBoundingClientRect();
  containerSize.value = { width: rect.width, height: rect.height };
  const ro = new ResizeObserver((entries) => {
    for (const entry of entries) {
      const cr = entry.contentRect;
      containerSize.value = { width: cr.width, height: cr.height };
    }
  });
  ro.observe(el);
  observer = ro;
  onCleanup(() => {
    ro.disconnect();
    if (observer === ro) observer = null;
  });
});

onBeforeUnmount(() => {
  if (observer) {
    observer.disconnect();
    observer = null;
  }
});

const layout = computed(() => {
  return selectGridLayout(
    tiles.value.length,
    containerSize.value.width,
    containerSize.value.height,
  );
});

const visibleTiles = computed<Tile[]>(() => {
  // When the participant count exceeds the layout's capacity (e.g.
  // 30 participants in a 5×5), we clip to the first `maxTiles`. The
  // "+N more" affordance lives in a future pagination pass — for
  // now we show as many as the layout supports.
  return tiles.value.slice(0, layout.value.maxTiles);
});

const gridStyle = computed(() => ({
  gridTemplateColumns: `repeat(${layout.value.cols}, minmax(0, 1fr))`,
  gridTemplateRows: `repeat(${layout.value.rows}, minmax(0, 1fr))`,
}));
</script>

<template>
  <div class="call-tile-grid">
    <!-- Speaker-focus layout: one large tile + horizontal thumb strip. -->
    <template v-if="focusedTile">
      <div class="call-tile-grid__focus">
        <CallTile
          :key="focusedTile.key"
          :label="focusedTile.label"
          :attach-key="focusedTile.key"
          :is-self="focusedTile.isSelf"
          :mirror-video="focusedTile.mirrorVideo"
          :shows-presenting-glyph="focusedTile.showsPresentingGlyph"
          :mic-enabled="focusedTile.micEnabledHint"
          :video-track="focusedTile.videoTrack"
          :audio-track="focusedTile.audioTrack"
          :attach="attach"
          class="call-tile--focused"
          @activate="toggleFocus(focusedTile)"
        />
      </div>
      <div v-if="otherTiles.length" class="call-tile-grid__thumbs">
        <CallTile
          v-for="tile in otherTiles"
          :key="tile.key"
          :label="tile.label"
          :attach-key="tile.key"
          :is-self="tile.isSelf"
          :mirror-video="tile.mirrorVideo"
          :shows-presenting-glyph="tile.showsPresentingGlyph"
          :mic-enabled="tile.micEnabledHint"
          :video-track="tile.videoTrack"
          :audio-track="tile.audioTrack"
          :attach="attach"
          class="call-tile--thumb"
          @activate="toggleFocus(tile)"
        />
      </div>
    </template>

    <!-- Adaptive equal-weight grid (default). Cells are `1fr × 1fr`
         from the JS-picked (cols, rows); tiles fill their cell with
         no per-tile aspect-ratio so a vertical splitter drag reflows
         tile shape without scrolling. -->
    <div
      v-else
      :ref="setGridEl"
      class="call-tile-grid__equal"
      :style="gridStyle"
    >
      <CallTile
        v-for="tile in visibleTiles"
        :key="tile.key"
        :label="tile.label"
        :attach-key="tile.key"
        :is-self="tile.isSelf"
        :mirror-video="tile.mirrorVideo"
        :shows-presenting-glyph="tile.showsPresentingGlyph"
        :mic-enabled="tile.micEnabledHint"
        :video-track="tile.videoTrack"
        :audio-track="tile.audioTrack"
        :attach="attach"
        @activate="toggleFocus(tile)"
      />
    </div>
  </div>
</template>

<style scoped>
.call-tile-grid {
  display: flex;
  height: 100%;
  width: 100%;
  flex-direction: column;
  gap: var(--space-xs);
  overflow: hidden;
}

.call-tile-grid__equal {
  display: grid;
  height: 100%;
  width: 100%;
  gap: var(--space-xs);
  padding: var(--space-xs);
  /* `grid-template-columns` / `grid-template-rows` are set inline by
   * the script. The cells are `minmax(0, 1fr)` (not `1fr`) so they
   * can shrink below the video's intrinsic min-content during a
   * splitter drag — without the `0` floor, grid would overflow. */
  overflow: hidden;
}

.call-tile-grid__focus {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  padding: var(--space-xs);
}

.call-tile-grid__thumbs {
  display: flex;
  gap: var(--space-xs);
  overflow-x: auto;
  flex-shrink: 0;
  padding: 0 var(--space-xs) var(--space-xs);
  max-height: 6.5rem;
}

/* The `.call-tile--focused` and `.call-tile--thumb` variants are
 * styled inside CallTile.vue (where they override the default 16:9
 * cap). The grid is only responsible for placing the right CallTile
 * variants inside the right wrapper. */
</style>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { css } from "styled-system/css";
import type { LocalMediaTrack, RemoteMediaTrack } from "@/lib/calls/engine";
import { TileAttachments, type TileAttachable } from "@/lib/calls/tile-attach";
import { selectGridLayout } from "@/lib/calls/grid-layout";
import { partitionStageTiles } from "@/lib/calls/stage-overflow";
import type { CallTileModel } from "@/lib/calls/call-tiles";
import { projectCallTiles, reconcileCallTileProjectionState } from "@/lib/calls/call-tile-projection";
import { highlightedTileKeys } from "@/lib/calls/active-speakers";
import { fullJidIdentityKey } from "@/lib/xmpp/jid";
import {
  $callPinnedTileKey,
  $callSelfViewHidden,
  $callViewMode,
  toggleCallPin,
} from "@/lib/calls/call-view-state";
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

/** Stable fallback so the highlight computed doesn't churn when no speaker
 *  prop is supplied. */
const EMPTY_ACTIVE_SPEAKERS: ReadonlySet<string> = new Set();

const props = defineProps<{
  remoteTracks: readonly RemoteMediaTrack[];
  localTracks: readonly LocalMediaTrack[];
  /** LiveKit identity for the local participant (may be null pre-connect). */
  localIdentity: string | null;
  /** Remote participants known from call state before LiveKit has subscribed to their tracks. */
  expectedRemoteIdentities?: readonly string[];
  /** Whether the local mic is currently un-muted. */
  micEnabled: boolean;
  /** Identities LiveKit currently reports (held) as actively speaking. */
  activeSpeakerIdentities?: ReadonlySet<string>;
  /** Participant the Speaker layout auto-promotes to the large tile. */
  promotedSpeakerIdentity?: string | null;
  /** Identity keys (`fullJidIdentityKey`) of participants with a raised
   *  hand (#1029); their camera tile shows the raised-hand badge. */
  raisedHandKeys?: ReadonlySet<string>;
  /** Identity keys (`fullJidIdentityKey`) of remote participants muted via
   *  `urn:waddle:in-call:0` presence (#1030); their tile mic hint is off. */
  mutedKeys?: ReadonlySet<string>;
}>();

const emit = defineEmits<{
  /** The "+N more" Overflow tile was activated; the surface opens the
   *  Participants panel (Split bumps to Expanded first). */
  openParticipants: [];
}>();

const seenRemoteScreenTrackKeys = ref<ReadonlySet<string>>(new Set());
// View mode + pin are call-scoped atoms (not local refs) so they survive the
// split⟷expanded surface switch that remounts this grid.
const viewMode = useStore($callViewMode);
const pinnedTileKey = useStore($callPinnedTileKey);
const selfViewHidden = useStore($callSelfViewHidden);

const projection = computed(() => {
  return projectCallTiles({
    remoteTracks: props.remoteTracks,
    localTracks: props.localTracks,
    localIdentity: props.localIdentity,
    expectedRemoteIdentities: props.expectedRemoteIdentities,
    micEnabled: props.micEnabled,
    mutedKeys: props.mutedKeys,
    seenRemoteScreenTrackKeys: seenRemoteScreenTrackKeys.value,
    pinnedTileKey: pinnedTileKey.value,
    viewMode: viewMode.value,
    promotedSpeakerIdentity: props.promotedSpeakerIdentity ?? null,
    hideSelfView: selfViewHidden.value,
  });
});

watch(projection, (next) => {
  const reconciled = reconcileCallTileProjectionState({
    tiles: next.tiles,
    pinnedTileKey: pinnedTileKey.value,
    currentSeenRemoteScreenTrackKeys: seenRemoteScreenTrackKeys.value,
    nextSeenRemoteScreenTrackKeys: next.seenRemoteScreenTrackKeys,
  });
  if (seenRemoteScreenTrackKeys.value !== reconciled.seenRemoteScreenTrackKeys) {
    seenRemoteScreenTrackKeys.value = reconciled.seenRemoteScreenTrackKeys;
  }
  // A pinned participant who has left is cleared so the pin can't reapply if
  // they rejoin; written back through the atom (the store is the source of
  // truth, not a local ref).
  if (pinnedTileKey.value !== reconciled.pinnedTileKey) {
    $callPinnedTileKey.set(reconciled.pinnedTileKey);
  }
}, { immediate: true });

const tiles = computed<Tile[]>(() => projection.value.tiles);
const focusedTile = computed<Tile | null>(() => {
  const spotlightKey = projection.value.spotlightKey;
  if (!spotlightKey) return null;
  return tiles.value.find((t) => t.key === spotlightKey) ?? null;
});
const otherTiles = computed<Tile[]>(() => {
  if (!focusedTile.value) return [];
  return tiles.value.filter((t) => t.key !== focusedTile.value?.key);
});

function toggleFocus(tile: Tile): void {
  toggleCallPin(tile.key);
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

// Tile keys that carry the active-speaker highlight in Gallery view. The pure
// `highlightedTileKeys` maps held speaking identities → their camera tile.
const speakingTileKeys = computed<ReadonlySet<string>>(() =>
  highlightedTileKeys(tiles.value, props.activeSpeakerIdentities ?? EMPTY_ACTIVE_SPEAKERS),
);

// Tile keys that carry the raised-hand badge (#1029). The raised-hand
// store is keyed by `fullJidIdentityKey`, so normalize each tile's raw
// LiveKit identity before matching. Like the speaker highlight, the badge
// rides the person's camera tile, not their screen-share tile.
const raisedHandTileKeys = computed<ReadonlySet<string>>(() => {
  const raised = props.raisedHandKeys ?? EMPTY_ACTIVE_SPEAKERS;
  if (raised.size === 0) return EMPTY_ACTIVE_SPEAKERS;
  const keys = new Set<string>();
  for (const tile of tiles.value) {
    if (tile.source !== "camera") continue;
    if (raised.has(fullJidIdentityKey(tile.identity))) keys.add(tile.key);
  }
  return keys;
});

// When the tile count exceeds the layout's capacity, the last cell becomes a
// "+N more" Overflow tile instead of silently dropping people. `+N` counts the
// participants who have no tile on the Stage; audio for them stays subscribed
// via the separate CallAudioSink, and their unrendered video auto-pauses under
// `adaptiveStream`/`dynacast`. The grid stays structured so true Gallery
// pagination can replace the single overflow cell later.
const stagePartition = computed(() =>
  partitionStageTiles(tiles.value, layout.value.maxTiles),
);
const visibleTiles = computed<Tile[]>(() => stagePartition.value.tiles);
const overflow = computed(() => stagePartition.value.overflow);

function activateOverflow(): void {
  emit("openParticipants");
}

// HuddleStage grid: 10px gaps inside the stage padding; the speaker view is
// one large tile over a strip of thumbnails.
const rootClass = css({ display: "flex", height: "100%", width: "100%", flexDirection: "column", gap: "2.5", overflow: "hidden", padding: "0 10px 6px", _narrow: { padding: "0 6px 2px", gap: "2" } });
const equalClass = css({ display: "grid", height: "100%", width: "100%", gap: "2.5", padding: "6px", overflow: "hidden", _narrow: { gap: "2" } });
const focusClass = css({ flex: "1 1 auto", minHeight: 0, display: "flex", padding: "6px" });
const thumbsClass = css({ display: "flex", gap: "2.5", overflowX: "auto", flexShrink: 0, padding: "6px", maxHeight: "116px" });
const thumbClass = css({ width: "144px", aspectRatio: "16 / 9", flex: "0 0 auto" });
const overflowClass = css({
  display: "flex",
  flexDirection: "column",
  alignItems: "center",
  justifyContent: "center",
  gap: "1",
  minWidth: 0,
  minHeight: 0,
  border: 0,
  borderRadius: "12px",
  bg: "surface.2",
  color: "fg",
  cursor: "pointer",
  _hover: { bgImage: "linear-gradient({colors.state.hover}, {colors.state.hover})" },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "2px" },
});
const overflowCountClass = css({ font: "700 22px/1 {fonts.display}" });
const overflowLabelClass = css({ fontSize: "sm", color: "fg.muted" });

const gridStyle = computed(() => ({
  gridTemplateColumns: `repeat(${layout.value.cols}, minmax(0, 1fr))`,
  gridTemplateRows: `repeat(${layout.value.rows}, minmax(0, 1fr))`,
}));
</script>

<template>
  <div :class="rootClass">
    <!-- Speaker-focus layout: one large tile + horizontal thumb strip. -->
    <template v-if="focusedTile">
      <div :class="focusClass">
        <CallTile
          :key="focusedTile.key"
          :label="focusedTile.label"
          :avatar-jid="focusedTile.identity"
          :attach-key="focusedTile.key"
          :is-self="focusedTile.isSelf"
          :mirror-video="focusedTile.mirrorVideo"
          :shows-presenting-glyph="focusedTile.showsPresentingGlyph"
          :mic-enabled="focusedTile.micEnabledHint"
          :video-track="focusedTile.videoTrack"
          :raised-hand="raisedHandTileKeys.has(focusedTile.key)"
          :attach="attach"
          @activate="toggleFocus(focusedTile)"
        />
      </div>
      <div v-if="otherTiles.length" :class="thumbsClass">
        <div v-for="tile in otherTiles" :key="tile.key" :class="thumbClass">
        <CallTile
          :label="tile.label"
          :avatar-jid="tile.identity"
          :attach-key="tile.key"
          :is-self="tile.isSelf"
          :mirror-video="tile.mirrorVideo"
          :shows-presenting-glyph="tile.showsPresentingGlyph"
          :mic-enabled="tile.micEnabledHint"
          :video-track="tile.videoTrack"
          :raised-hand="raisedHandTileKeys.has(tile.key)"
          :attach="attach"
          @activate="toggleFocus(tile)"
        />
        </div>
      </div>
    </template>

    <!-- Adaptive equal-weight grid (default). Cells are `1fr × 1fr`
         from the JS-picked (cols, rows); tiles fill their cell with
         no per-tile aspect-ratio so a vertical splitter drag reflows
         tile shape without scrolling. -->
    <div
      v-else
      :ref="setGridEl"
      :class="equalClass"
      :style="gridStyle"
    >
      <CallTile
        v-for="tile in visibleTiles"
        :key="tile.key"
        :label="tile.label"
        :avatar-jid="tile.identity"
        :attach-key="tile.key"
        :is-self="tile.isSelf"
        :mirror-video="tile.mirrorVideo"
        :shows-presenting-glyph="tile.showsPresentingGlyph"
        :mic-enabled="tile.micEnabledHint"
        :video-track="tile.videoTrack"
        :speaking="speakingTileKeys.has(tile.key)"
        :raised-hand="raisedHandTileKeys.has(tile.key)"
        :attach="attach"
        @activate="toggleFocus(tile)"
      />
      <button
        v-if="overflow"
        type="button"
        :class="['call-tile-grid__overflow', overflowClass]"
        :aria-label="`Show ${overflow.hiddenCount} more ${overflow.hiddenCount === 1 ? 'participant' : 'participants'}`"
        @click="activateOverflow"
      >
        <span :class="overflowCountClass">+{{ overflow.hiddenCount }}</span>
        <span :class="overflowLabelClass">more</span>
      </button>
    </div>
  </div>
</template>

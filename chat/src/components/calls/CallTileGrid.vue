<script setup lang="ts">
import { computed, inject, ref } from "vue";
import { MicOff } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { LocalMediaTrack, RemoteMediaTrack } from "@/lib/calls/engine";
import { callVideoRegistryKey, type CallVideoRegistry } from "@/lib/calls/video-registry";
import { TileAttachments, type TileAttachable } from "@/lib/calls/tile-attach";

/**
 * One tile = one (identity, role) slot in the grid. Each
 * participant contributes a single tile; the optional video track
 * fills the surface, and the audio track (when remote) is mounted
 * off-screen so playback works even if the user has the video off.
 */
type Tile = {
  key: string;
  identity: string;
  label: string;
  isSelf: boolean;
  micEnabledHint: boolean;
  videoTrack: {
    attach: (el: HTMLMediaElement) => void;
    detach: (el: HTMLMediaElement) => HTMLMediaElement;
  } | null;
  audioTrack: {
    attach: (el: HTMLMediaElement) => void;
    detach: (el: HTMLMediaElement) => HTMLMediaElement;
  } | null;
};

const props = defineProps<{
  remoteTracks: RemoteMediaTrack[];
  localTracks: LocalMediaTrack[];
  /** LiveKit identity for the local participant (may be null pre-connect). */
  localIdentity: string | null;
  /** Whether the local mic is currently un-muted. Drives the
   *  `MicOff` badge on the self-tile. */
  micEnabled: boolean;
}>();

/**
 * The tile the user has explicitly clicked. When set, the grid
 * pivots to a speaker-focused layout: the focused tile takes the
 * dominant area, every other tile becomes a small thumbnail along
 * the bottom. Clicking the focused tile or the empty area dismisses
 * focus. Default (`null`) is the equal-weight grid.
 */
const focusedKey = ref<string | null>(null);

function displayNameForIdentity(identity: string): string {
  const at = identity.indexOf("@");
  if (at <= 0) return identity;
  return identity.slice(0, at);
}

const tiles = computed<Tile[]>(() => {
  const byIdentity = new Map<string, Tile>();
  // Local participant always anchors slot 0 so the user sees a
  // self-preview even pre-publish ("yes, the call is alive and
  // pointed at me"). Prefer the first local track's identity over
  // `props.localIdentity` because the parent reads it from a
  // non-reactive getter (`engine.localIdentity`) and may pass null on
  // the first render; falling back to "you" then minting a separate
  // tile under the real identity when tracks land would render the
  // local participant twice.
  const selfId =
    props.localTracks[0]?.participantIdentity ?? props.localIdentity ?? "you";
  byIdentity.set(selfId, {
    key: `self:${selfId}`,
    identity: selfId,
    label: "You",
    isSelf: true,
    micEnabledHint: props.micEnabled,
    videoTrack: null,
    audioTrack: null,
  });
  for (const local of props.localTracks) {
    const existing = byIdentity.get(local.participantIdentity) ?? {
      key: `self:${local.participantIdentity}`,
      identity: local.participantIdentity,
      label: "You",
      isSelf: true,
      micEnabledHint: props.micEnabled,
      videoTrack: null,
      audioTrack: null,
    };
    if (local.kind === "video") existing.videoTrack = local.track;
    // Suppress local mic playback to avoid echo.
    byIdentity.set(local.participantIdentity, existing);
  }
  for (const remote of props.remoteTracks) {
    const existing = byIdentity.get(remote.participantIdentity) ?? {
      key: `remote:${remote.participantIdentity}`,
      identity: remote.participantIdentity,
      label: displayNameForIdentity(remote.participantIdentity),
      isSelf: false,
      micEnabledHint: true,
      videoTrack: null,
      audioTrack: null,
    };
    if (remote.kind === "video") existing.videoTrack = remote.track;
    if (remote.kind === "audio") existing.audioTrack = remote.track;
    byIdentity.set(remote.participantIdentity, existing);
  }
  return Array.from(byIdentity.values());
});

const focusedTile = computed<Tile | null>(() => {
  if (!focusedKey.value) return null;
  return tiles.value.find((t) => t.key === focusedKey.value) ?? null;
});

const otherTiles = computed<Tile[]>(() => {
  if (!focusedTile.value) return [];
  const focusKey = focusedTile.value.key;
  return tiles.value.filter((t) => t.key !== focusKey);
});

function toggleFocus(tile: Tile) {
  focusedKey.value = focusedKey.value === tile.key ? null : tile.key;
}

/**
 * Optional injected registry shared with CallOverlay so PIP can look
 * up a tile's `<video>` element without crossing the LiveKit SDK's
 * private `attachedElements` surface. Resolved at component setup;
 * `null` when no overlay provided one (e.g. standalone snapshots in
 * tests).
 */
const videoRegistry = inject<CallVideoRegistry | null>(callVideoRegistryKey, null);

/**
 * Per-tile bookkeeping for the (element, track) pair currently
 * attached. See `tile-attach.ts` for the contract — short version:
 * every `:ref` change goes through `sync` which reconciles the
 * previous record so LiveKit's `attachedElements` array never grows
 * stale entries.
 */
const attachments = new TileAttachments();

function attachTrack(
  key: string,
  el: HTMLMediaElement | null,
  track: TileAttachable | null,
  identity: string | null,
): void {
  attachments.sync(key, el, track, identity, videoRegistry);
}
</script>

<template>
  <div class="call-tile-grid">
    <!-- Speaker-focus layout: one big tile, thumbnails below. -->
    <template v-if="focusedTile">
      <div class="call-tile-grid__focus">
        <div
          :key="focusedTile.key"
          class="call-tile call-tile--focused"
          role="button"
          tabindex="0"
          @click="toggleFocus(focusedTile)"
          @keydown.enter="toggleFocus(focusedTile)"
        >
          <video
            v-if="focusedTile.videoTrack"
            :ref="(el) => attachTrack(`${focusedTile?.key}:video`, el as HTMLVideoElement | null, focusedTile?.videoTrack ?? null, focusedTile?.identity ?? null)"
            class="call-tile__video"
            :class="focusedTile.isSelf ? 'call-tile__video--mirrored' : ''"
            autoplay
            playsinline
            :muted="focusedTile.isSelf"
          />
          <div v-else class="call-tile__avatar">
            <AppAvatar :name="focusedTile.label" size="lg" />
          </div>
          <audio
            v-if="focusedTile.audioTrack && !focusedTile.isSelf"
            :ref="(el) => attachTrack(`${focusedTile?.key}:audio`, el as HTMLAudioElement | null, focusedTile?.audioTrack ?? null, null)"
            autoplay
          />
          <div class="call-tile__nameplate">
            <span class="type-caption truncate">{{ focusedTile.label }}</span>
            <span
              v-if="focusedTile.isSelf && !focusedTile.micEnabledHint"
              class="call-tile__mic-off"
              aria-label="Your mic is muted"
            >
              <MicOff class="w-3.5 h-3.5" />
            </span>
          </div>
        </div>
      </div>
      <div v-if="otherTiles.length" class="call-tile-grid__thumbs">
        <div
          v-for="tile in otherTiles"
          :key="tile.key"
          class="call-tile call-tile--thumb"
          role="button"
          tabindex="0"
          @click="toggleFocus(tile)"
          @keydown.enter="toggleFocus(tile)"
        >
          <video
            v-if="tile.videoTrack"
            :ref="(el) => attachTrack(`${tile.key}:video`, el as HTMLVideoElement | null, tile.videoTrack, tile.identity)"
            class="call-tile__video"
            :class="tile.isSelf ? 'call-tile__video--mirrored' : ''"
            autoplay
            playsinline
            :muted="tile.isSelf"
          />
          <div v-else class="call-tile__avatar">
            <AppAvatar :name="tile.label" size="md" />
          </div>
          <audio
            v-if="tile.audioTrack && !tile.isSelf"
            :ref="(el) => attachTrack(`${tile.key}:audio`, el as HTMLAudioElement | null, tile.audioTrack, null)"
            autoplay
          />
          <div class="call-tile__nameplate">
            <span class="type-caption truncate">{{ tile.label }}</span>
            <span
              v-if="tile.isSelf && !tile.micEnabledHint"
              class="call-tile__mic-off"
              aria-label="Your mic is muted"
            >
              <MicOff class="w-3 h-3" />
            </span>
          </div>
        </div>
      </div>
    </template>

    <!-- Equal-weight grid layout (default). `auto-fit` minmax keeps
         each tile at a comfortable minimum width on wide screens
         while letting single-tile calls fill the available area. -->
    <template v-else>
      <div class="call-tile-grid__equal">
        <div
          v-for="tile in tiles"
          :key="tile.key"
          class="call-tile"
          role="button"
          tabindex="0"
          @click="toggleFocus(tile)"
          @keydown.enter="toggleFocus(tile)"
        >
          <video
            v-if="tile.videoTrack"
            :ref="(el) => attachTrack(`${tile.key}:video`, el as HTMLVideoElement | null, tile.videoTrack, tile.identity)"
            class="call-tile__video"
            :class="tile.isSelf ? 'call-tile__video--mirrored' : ''"
            autoplay
            playsinline
            :muted="tile.isSelf"
          />
          <div v-else class="call-tile__avatar">
            <AppAvatar :name="tile.label" size="lg" />
          </div>
          <audio
            v-if="tile.audioTrack && !tile.isSelf"
            :ref="(el) => attachTrack(`${tile.key}:audio`, el as HTMLAudioElement | null, tile.audioTrack, null)"
            autoplay
          />
          <div class="call-tile__nameplate">
            <span class="type-caption truncate">{{ tile.label }}</span>
            <span
              v-if="tile.isSelf && !tile.micEnabledHint"
              class="call-tile__mic-off"
              aria-label="Your mic is muted"
            >
              <MicOff class="w-3.5 h-3.5" />
            </span>
          </div>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped>
.call-tile-grid {
  display: flex;
  height: 100%;
  flex-direction: column;
  gap: var(--space-xs);
  overflow: hidden;
}

.call-tile-grid__equal {
  display: grid;
  height: 100%;
  gap: var(--space-xs);
  grid-template-columns: repeat(auto-fit, minmax(min(180px, 100%), 1fr));
  grid-auto-rows: 1fr;
  overflow-y: auto;
  padding: var(--space-xs);
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
  padding: var(--space-xs);
  max-height: 5.5rem;
}

.call-tile {
  position: relative;
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--muted);
  aspect-ratio: 16 / 9;
  cursor: pointer;
  transition: box-shadow 0.18s ease, transform 0.18s ease;
}

.call-tile:hover {
  box-shadow: 0 0 18px var(--glow);
}

.call-tile:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}

.call-tile--focused {
  flex: 1 1 auto;
  aspect-ratio: unset;
  width: 100%;
}

.call-tile--thumb {
  flex: 0 0 auto;
  width: 9rem;
}

.call-tile__video {
  height: 100%;
  width: 100%;
  object-fit: cover;
}

.call-tile__video--mirrored {
  transform: scaleX(-1);
}

.call-tile__avatar {
  display: flex;
  height: 100%;
  width: 100%;
  align-items: center;
  justify-content: center;
}

.call-tile__nameplate {
  position: absolute;
  inset-inline: 0;
  bottom: 0;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-xs);
  background: linear-gradient(to top, rgba(0, 0, 0, 0.6), transparent);
  color: white;
  padding: 0.375rem 0.5rem;
}

.call-tile__mic-off {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--destructive-foreground);
  background: var(--destructive);
  border-radius: 999px;
  width: 1.25rem;
  height: 1.25rem;
}
</style>

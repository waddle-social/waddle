<script setup lang="ts">
import { computed } from "vue";
import { MicOff, ScreenShare } from "lucide-vue-next";
import { consistentColor } from "@/lib/chat-ui";
import type { TileAttachable } from "@/lib/calls/tile-attach";

/**
 * One participant tile inside the call grid.
 *
 * Structure mirrors LiveKit's `ParticipantTile`: two stacked
 * absolute layers — the placeholder paints the gradient + initial
 * underneath, the `<video>` paints over it when a track is
 * attached. When the video drops (camera off mid-call), the video
 * element unmounts and the placeholder fades back in.
 *
 * The tile itself is a `position: relative` container with
 * `container-type: size`; the initial scales via `cqmin` so it grows
 * with the tile regardless of grid shape (wide cell vs tall cell).
 *
 * Track attachment is delegated back to the parent through
 * callbacks: the parent owns the single `TileAttachments` instance
 * so LiveKit's `attachedElements` array stays consistent.
 */
const props = defineProps<{
  /** Human label rendered in the nameplate and derived into initials. */
  label: string;
  /** Stable identity-derived key namespace for `TileAttachments.sync`.
   *  The tile builds `${attachKey}:video` / `${attachKey}:audio` so
   *  one attachments map can host video and audio for the same
   *  participant without collision. */
  attachKey: string;
  /** Whether this tile represents the local participant. */
  isSelf: boolean;
  /** Whether the video should be mirrored. Used for local camera only,
   *  not local screen-share previews. */
  mirrorVideo: boolean;
  /** Whether to show the presenting affordance for screen tiles. */
  showsPresentingGlyph: boolean;
  /** Whether the local mic is currently un-muted. Drives the
   *  destructive `MicOff` badge on the self-tile. Ignored on remotes. */
  micEnabled: boolean;
  /** Optional video track to render. When `null`, the placeholder
   *  layer shows through unchanged. */
  videoTrack: TileAttachable | null;
  /** Optional remote audio track. Self-tiles never play audio (echo). */
  audioTrack: TileAttachable | null;
  /** Parent's attach reconciler — see `tile-attach.ts`. The tile
   *  invokes this from its `<video>` / `<audio>` ref callbacks. */
  attach: (key: string, el: HTMLMediaElement | null, track: TileAttachable | null) => void;
}>();

defineEmits<{
  /** Click on the tile. Used by the parent to enter / exit speaker focus. */
  activate: [];
}>();

const initials = computed<string>(() => {
  return props.label
    .split(/\s+/)
    .map((part) => part[0])
    .filter(Boolean)
    .join("")
    .toUpperCase()
    .slice(0, 2);
});

const placeholderBackground = computed<string>(() => {
  const base = consistentColor(props.label, 55, 45);
  return `radial-gradient(circle at 32% 26%, ` +
    `color-mix(in oklab, ${base}, white 22%), ` +
    `${base} 58%, ` +
    `color-mix(in oklab, ${base}, black 14%))`;
});

function refVideo(el: Element | null): void {
  props.attach(
    `${props.attachKey}:video`,
    el instanceof HTMLVideoElement ? el : null,
    props.videoTrack,
  );
}

function refAudio(el: Element | null): void {
  props.attach(
    `${props.attachKey}:audio`,
    el instanceof HTMLAudioElement ? el : null,
    props.audioTrack,
  );
}
</script>

<template>
  <div
    class="call-tile"
    :class="{ 'call-tile--has-video': videoTrack !== null }"
    role="button"
    tabindex="0"
    :aria-label="`Open ${label} tile`"
    @click="$emit('activate')"
    @keydown.enter="$emit('activate')"
  >
    <!-- Placeholder layer — always mounted so the tile never shows a
         bare cell while LiveKit is still subscribing to the track.
         Fades out under the video when the track lands. -->
    <div
      class="call-tile__placeholder"
      :style="{ background: placeholderBackground }"
    >
      <span class="call-tile__avatar-disc" aria-hidden="true">
        {{ initials }}
      </span>
    </div>

    <video
      v-if="videoTrack"
      :ref="refVideo"
      class="call-tile__video"
      :class="{ 'call-tile__video--mirrored': mirrorVideo }"
      autoplay
      playsinline
      :muted="isSelf"
    />

    <audio
      v-if="audioTrack && !isSelf"
      :ref="refAudio"
      autoplay
    />

    <!-- Persistent nameplate — same look whether a video is playing
         or the placeholder is showing, so the tile reads identically
         across states. -->
    <div class="call-tile__nameplate">
      <span class="call-tile__name">
        <ScreenShare
          v-if="showsPresentingGlyph"
          class="call-tile__presenting-icon"
          aria-hidden="true"
        />
        {{ label }}
      </span>
      <span
        v-if="isSelf && !micEnabled"
        class="call-tile__mic-off"
        aria-label="Your mic is muted"
      >
        <MicOff class="w-3.5 h-3.5" />
      </span>
    </div>
  </div>
</template>

<style scoped>
.call-tile {
  /* The tile FILLS its grid cell (no aspect-ratio cap, no margin
   * auto). The video inside uses `object-fit: contain` so faces are
   * never cropped no matter how off-aspect the cell is during a
   * splitter drag; letterbox bands show the gradient placeholder
   * underneath, which keeps the tile visually substantial instead
   * of revealing the muted background.
   *
   * Container queries on the tile scale the placeholder initial
   * with the tile's actual rendered size. */
  position: relative;
  overflow: hidden;
  border-radius: var(--radius-md);
  border: 1px solid var(--border);
  background: var(--muted);
  cursor: pointer;
  width: 100%;
  height: 100%;
  min-width: 0;
  min-height: 0;
  container-type: size;
  isolation: isolate;
  transition: box-shadow 160ms ease;
}

/* Thumbnail variant — fixed-width 16:9 strip preview in the
 * speaker-focus filmstrip; height derives from width via
 * aspect-ratio. */
.call-tile.call-tile--thumb {
  width: 9rem;
  height: auto;
  aspect-ratio: 16 / 9;
  flex: 0 0 auto;
}

.call-tile:hover {
  box-shadow: 0 0 18px var(--glow);
}

.call-tile:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}

/* The placeholder layer is ALWAYS painted underneath the video.
 * When the video lands, only the avatar disc fades out — the
 * gradient bg stays so it shows through the video's letterbox
 * bands (object-fit: contain on the video). This keeps the tile
 * visually substantial regardless of cell aspect. */
.call-tile__placeholder {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  box-shadow: inset 0 0 120px color-mix(in oklab, black 28%, transparent);
}

.call-tile__avatar-disc {
  transition: opacity 180ms ease-out;
}

.call-tile--has-video .call-tile__avatar-disc {
  opacity: 0;
}

.call-tile__avatar-disc {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: min(36cqmin, 9rem);
  height: min(36cqmin, 9rem);
  border-radius: 9999px;
  background: color-mix(in oklab, white 15%, transparent);
  backdrop-filter: blur(6px);
  -webkit-backdrop-filter: blur(6px);
  font-weight: 700;
  letter-spacing: 0.02em;
  font-size: clamp(1.25rem, 14cqmin, 4rem);
  text-shadow: 0 1px 2px color-mix(in oklab, black 40%, transparent);
  user-select: none;
}

.call-tile__video {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  /* `contain` so the camera's full frame is always visible — faces
   * are never cropped, no matter the cell aspect. When the cell is
   * off-aspect (e.g. a wide-short splitter drag), the resulting
   * letterbox bands reveal the gradient placeholder painted in the
   * layer below, so the tile reads as a unified frame instead of
   * a video stranded on a muted background. */
  object-fit: contain;
  object-position: center;
  /* Transparent so the placeholder gradient is visible in any
   * letterbox region the contain-fit produces. */
  background: transparent;
}

.call-tile__video--mirrored {
  /* Local front-facing camera is always mirrored for natural feel.
   * Screen-share readability is decided by the tile projection before
   * this renderer sees the track. */
  transform: scaleX(-1);
}

.call-tile__nameplate {
  position: absolute;
  inset-inline: 0;
  bottom: 0;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-xs);
  padding: 0.375rem 0.5rem;
  background: linear-gradient(to top, rgba(0, 0, 0, 0.55), transparent);
  color: white;
  pointer-events: none;
}

.call-tile__name {
  display: inline-flex;
  align-items: center;
  gap: 0.25rem;
  min-width: 0;
  font-size: 0.8125rem;
  line-height: 1;
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  text-shadow: 0 1px 2px color-mix(in oklab, black 60%, transparent);
}

.call-tile__presenting-icon {
  width: 0.875rem;
  height: 0.875rem;
  flex: 0 0 auto;
  filter: drop-shadow(0 1px 1px color-mix(in oklab, black 55%, transparent));
}

.call-tile__mic-off {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.25rem;
  height: 1.25rem;
  border-radius: 9999px;
  background: var(--destructive);
  color: var(--destructive-foreground);
  flex-shrink: 0;
}
</style>

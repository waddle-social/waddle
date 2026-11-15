<script setup lang="ts">
import { computed } from "vue";
import { Hand, MicOff, ScreenShare } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { avatar, plate } from "styled-system/recipes";
import { useAvatarUrl } from "@/lib/avatars/use-avatar-url";
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
const props = withDefaults(defineProps<{
  /** Human label rendered in the nameplate and derived into initials. */
  label: string;
  /** Participant JID whose avatar fills the placeholder disc; initials otherwise. */
  avatarJid?: string | null;
  /** Stable identity-derived key namespace for `TileAttachments.sync`. */
  attachKey: string;
  /** Whether this tile represents the local participant. */
  isSelf: boolean;
  /** Whether the video should be mirrored. Used for local camera only,
   *  not local screen-share previews. */
  mirrorVideo: boolean;
  /** Whether to show the presenting affordance for screen tiles. */
  showsPresentingGlyph: boolean;
  /** Whether this tile's mic is currently un-muted. Drives the
   *  destructive `MicOff` badge. Authoritative for both self (the local
   *  mic flag) and remotes (presence-advertised mute, #1030). */
  micEnabled: boolean;
  /** Optional video track to render. When `null`, the placeholder
   *  layer shows through unchanged. */
  videoTrack: TileAttachable | null;
  /** Parent's attach reconciler — see `tile-attach.ts`. The tile
   *  invokes this from its `<video>` ref callback. */
  attach: (key: string, el: HTMLMediaElement | null, track: TileAttachable | null) => void;
  interactive?: boolean;
  /** Whether this participant is the (held) active speaker. Drives the
   *  highlight border in Gallery view. */
  speaking?: boolean;
  /** Whether this participant advertises a raised hand (#1029). Drives
   *  the nameplate hand badge. */
  raisedHand?: boolean;
}>(), {
  interactive: true,
  speaking: false,
  raisedHand: false,
});

const avatarUrl = useAvatarUrl(() => props.avatarJid);

const initials = computed<string>(() => {
  return props.label
    .split(/\s+/)
    .map((part) => part[0])
    .filter(Boolean)
    .join("")
    .toUpperCase()
    .slice(0, 2);
});

// Name-derived avatar tint (same hash as AppAvatar), so a person keeps
// one colour across the sidebar, timeline and stage.
const tint = computed(() => {
  let hash = 5381;
  for (const ch of props.label) hash = (Math.imul(hash, 33) + (ch.codePointAt(0) ?? 0)) >>> 0;
  return String((hash % 6) + 1) as "1" | "2" | "3" | "4" | "5" | "6";
});

function refVideo(el: Element | null): void {
  props.attach(
    `${props.attachKey}:video`,
    el instanceof HTMLVideoElement ? el : null,
    props.videoTrack,
  );
}

function activate(): void {
  if (props.interactive) emit("activate");
}

const emit = defineEmits<{
  /** Click on the tile. Used by the parent to enter / exit speaker focus. */
  activate: [];
}>();

// HuddleStage tile: rounded, surface ground with a tinted avatar disc, a
// live ring while speaking, plate bottom-left, raised hand top-left. The
// tile fills its grid cell; the disc scales with the cell (`cqmin`).
const rootClass = css({
  position: "relative",
  overflow: "hidden",
  borderRadius: "12px",
  bg: "surface",
  boxShadow: "0 0 0 1px {colors.border.soft}",
  width: "100%",
  height: "100%",
  minWidth: 0,
  minHeight: 0,
  containerType: "size",
  isolation: "isolate",
  transition: "box-shadow {durations.fast} {easings.out}",
});
const interactiveClass = css({
  cursor: "pointer",
  _hover: { boxShadow: "0 0 0 1px {colors.border.strong}" },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "2px" },
});
const speakingClass = css({
  boxShadow: "0 0 0 2px {colors.live}, 0 0 0 6px color-mix(in srgb, {colors.live} 22%, transparent)",
  _hover: { boxShadow: "0 0 0 2px {colors.live}, 0 0 0 6px color-mix(in srgb, {colors.live} 22%, transparent)" },
  "@media (forced-colors: active)": { outline: "3px solid Highlight" },
});
const placeholderClass = css({ position: "absolute", inset: 0, display: "flex", alignItems: "center", justifyContent: "center" });
const discClass = css({
  width: "min(36cqmin, 96px)",
  height: "min(36cqmin, 96px)",
  fontSize: "clamp(12px, 13cqmin, 34px)",
  overflow: "hidden",
  userSelect: "none",
  transition: "opacity {durations.base} {easings.out}",
});
const discHiddenClass = css({ opacity: 0 });
const imageClass = css({ width: "100%", height: "100%", borderRadius: "pill", objectFit: "cover" });
const videoClass = css({ position: "absolute", inset: 0, width: "100%", height: "100%", objectFit: "contain", objectPosition: "center", bg: "transparent" });
const mirroredClass = css({ transform: "scaleX(-1)" });
const plateClass = cx(plate(), css({ position: "absolute", left: "8px", bottom: "8px", pointerEvents: "none", "& svg": { width: "14px", height: "14px", flex: "none" } }));
const nameClass = css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const mutedClass = css({ display: "inline-flex", color: "danger.text" });
const handClass = css({
  position: "absolute",
  top: "8px",
  left: "8px",
  display: "inline-flex",
  width: "28px",
  height: "28px",
  borderRadius: "pill",
  bg: "gold",
  color: "gold.fg",
  alignItems: "center",
  justifyContent: "center",
  "& svg": { width: "16px", height: "16px" },
});
</script>

<template>
  <div
    :class="[rootClass, interactive && interactiveClass, speaking && speakingClass]"
    :data-speaking="speaking ? 'true' : undefined"
    :role="interactive ? 'button' : 'img'"
    :tabindex="interactive ? 0 : undefined"
    :aria-label="interactive ? `Open ${label} tile` : label"
    :data-call-tile-key="attachKey"
    @click="activate"
    @keydown.enter="activate"
  >
    <!-- Placeholder layer — always mounted so the tile never shows a
         bare cell while LiveKit is still subscribing to the track. The
         disc fades out under the video when the track lands. -->
    <div :class="placeholderClass">
      <span :class="[avatar({ tint }), discClass, videoTrack !== null && discHiddenClass]" aria-hidden="true">
        <img v-if="avatarUrl" :src="avatarUrl" alt="" :class="imageClass" />
        <template v-else>{{ initials }}</template>
      </span>
    </div>

    <video
      v-if="videoTrack"
      :ref="refVideo"
      :class="[videoClass, mirrorVideo && mirroredClass]"
      :data-call-tile-key="attachKey"
      autoplay
      playsinline
      :muted="isSelf"
    />

    <span
      v-if="raisedHand"
      :class="handClass"
      aria-label="Hand raised"
    >
      <Hand aria-hidden="true" />
    </span>

    <!-- Persistent plate — same look whether a video is playing or the
         placeholder is showing. -->
    <span :class="plateClass">
      <ScreenShare v-if="showsPresentingGlyph" aria-hidden="true" />
      <span :class="nameClass">{{ label }}</span>
      <span
        v-if="!micEnabled"
        :class="mutedClass"
        :aria-label="isSelf ? 'Your mic is muted' : 'Muted'"
      >
        <MicOff aria-hidden="true" />
      </span>
    </span>
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { Avatar } from "@ark-ui/vue/avatar";
import { css, cx } from "styled-system/css";
import { avatar, presence as presenceRecipe } from "styled-system/recipes";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import type { OccupantPresence } from "@/lib/xmpp-client";

const props = defineProps<{
  name: string;
  src?: string | null;
  size?: "xs" | "sm" | "md" | "lg" | "xl" | "message";
  presence?: OccupantPresence;
  lastSeen?: number;
  /**
   * Whether the contact is in a call (XEP-0108 overlay, ADR-010 Phase 3).
   * Renders an "in a call" badge layered on top of the presence dot — it is
   * orthogonal to the Show and never replaces it. One badge for audio + video.
   */
  inCall?: boolean;
  /** In a huddle: a live (aurora) ring around the avatar. */
  huddle?: boolean;
  /** Speaking in a huddle: the ring glows. Implies `huddle`. */
  speaking?: boolean;
}>();

// Avatar sizes map onto the preset `avatar` recipe: xs 20, sm 28, md 36,
// lg 44, xl 64px. `message` follows the density-aware `msgAvatar` token.
type RecipeSize = "xs" | "sm" | "lg" | "xl" | undefined;
const RECIPE_SIZE: Record<NonNullable<typeof props.size>, RecipeSize> = {
  xs: "xs",
  sm: "sm",
  md: undefined,
  lg: "lg",
  xl: "xl",
  message: undefined,
};
const TINTS = ["1", "2", "3", "4", "5", "6"] as const;

const messageSize = css({ width: "msgAvatar", height: "msgAvatar" });
const huddleRing = css({ boxShadow: "0 0 0 2px {colors.live}" });
const offline = css({ "& > img, & > [data-part=fallback]": { opacity: 0.55, filter: "grayscale(0.4)" } });
// The image is never lazy-loaded: Ark keeps it `hidden` until it loads, and a
// hidden lazy image never loads, so a picture resolved after mount would stay
// on initials.
const imageClass = css({ width: "100%", height: "100%", borderRadius: "inherit", objectFit: "cover", bg: "surface.2" });
const fallbackClass = css({ userSelect: "none", lineHeight: 1 });
const callBadge = css({
  position: "absolute",
  top: "-2px",
  right: "-2px",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  width: "14px",
  height: "14px",
  borderRadius: "pill",
  border: "2px solid var(--ring-bg, {colors.bg})",
  bg: "live",
  color: "live.fg",
  "& svg": { width: "8px", height: "8px" },
});

const initials = computed(() =>
  props.name
    .split(" ")
    .map((n) => n[0])
    .join("")
    .toUpperCase()
    .slice(0, 2),
);

// Name-derived tint so a person keeps the same fallback colour everywhere.
const tint = computed(() => {
  let hash = 5381;
  for (const ch of props.name) hash = (Math.imul(hash, 33) + (ch.codePointAt(0) ?? 0)) >>> 0;
  return TINTS[hash % TINTS.length];
});

const rootClass = computed(() => cx(
  "app-avatar",
  avatar({ size: RECIPE_SIZE[props.size ?? "md"], tint: tint.value }),
  props.size === "message" && messageSize,
  props.huddle && !props.speaking && huddleRing,
  props.presence === "offline" && offline,
));

const presenceState = computed(() => {
  switch (props.presence) {
    case "online": return "available";
    case "away":   return "away";
    case "dnd":    return "dnd";
    case "offline": return "offline";
    default:       return null;
  }
});

function formatRelativeTime(timestamp: number): string {
  const seconds = Math.floor((Date.now() - timestamp) / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes === 1) return "1 minute ago";
  if (minutes < 60) return `${minutes} minutes ago`;
  const hours = Math.floor(minutes / 60);
  if (hours === 1) return "1 hour ago";
  if (hours < 24) return `${hours} hours ago`;
  const days = Math.floor(hours / 24);
  if (days === 1) return "yesterday";
  return `${days} days ago`;
}

const presenceTooltip = computed(() => {
  switch (props.presence) {
    case "online": return "Online";
    case "away":   return "Away";
    case "dnd":    return "Do not disturb";
    case "offline":
      return props.lastSeen
        ? `Last seen ${formatRelativeTime(props.lastSeen)}`
        : "Offline";
    default:       return undefined;
  }
});

const tooltipLabel = computed(() => {
  const parts = [presenceTooltip.value, props.inCall ? "In a call" : undefined].filter(Boolean);
  return parts.join(" · ");
});
</script>

<template>
  <AppTooltip :label="tooltipLabel">
    <Avatar.Root :class="rootClass" :data-speaking="speaking ? 'true' : undefined">
      <Avatar.Image v-if="src" :src="src" :alt="name" :class="imageClass" />
      <Avatar.Fallback :class="fallbackClass">{{ initials }}</Avatar.Fallback>
      <span
        v-if="presenceState"
        :class="presenceRecipe({ state: presenceState })"
        :data-presence="presenceState"
        aria-hidden="true"
      />
      <span v-if="inCall" :class="['app-avatar-call-badge', callBadge]" role="img" aria-label="In a call">
        <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <path d="M6.6 10.8a15.2 15.2 0 0 0 6.6 6.6l2.2-2.2a1 1 0 0 1 1-.25 11.4 11.4 0 0 0 3.6.57 1 1 0 0 1 1 1V20a1 1 0 0 1-1 1A17 17 0 0 1 3 4a1 1 0 0 1 1-1h3.5a1 1 0 0 1 1 1c0 1.25.2 2.46.57 3.6a1 1 0 0 1-.25 1z" />
        </svg>
      </span>
    </Avatar.Root>
  </AppTooltip>
</template>

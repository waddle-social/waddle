<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { consistentColor } from "@/lib/chat-ui";
import type { OccupantPresence } from "@/lib/xmpp-client";

const props = defineProps<{
  name: string;
  src?: string | null;
  size?: "xs" | "sm" | "md" | "lg" | "message";
  presence?: OccupantPresence;
  lastSeen?: number;
  /**
   * Whether the contact is in a call (XEP-0108 overlay, ADR-010 Phase 3).
   * Renders an "in a call" badge layered on top of the presence dot — it is
   * orthogonal to the Show and never replaces it. One badge for audio + video.
   */
  inCall?: boolean;
}>();

const imageFailed = ref(false);

const initials = computed(() =>
  props.name
    .split(" ")
    .map((n) => n[0])
    .join("")
    .toUpperCase()
    .slice(0, 2),
);

const sizeClass = computed(() => {
  if (props.size === "xs") return "w-6 h-6 type-avatar-xs";
  if (props.size === "sm") return "w-7 h-7 type-avatar-sm";
  if (props.size === "message") return "app-avatar-message-size type-avatar-md";
  if (props.size === "lg") return "w-12 h-12 type-avatar-lg";
  return "w-8 h-8 type-avatar-md";
});

const wrapperSizeClass = computed(() => {
  if (props.size === "xs") return "w-6 h-6";
  if (props.size === "sm") return "w-7 h-7";
  if (props.size === "message") return "app-avatar-message-size";
  if (props.size === "lg") return "w-12 h-12";
  return "w-8 h-8";
});

const bgColor = computed(() => consistentColor(props.name, 55, 45));

// Subtle radial gradient for the letter-fallback avatar — slight
// highlight up-and-left, base color in the body, slight shade
// bottom-right. Mixes in OKLab against the name-derived hue so each
// avatar keeps its identity while gaining a tiny "lit from above"
// dimensionality. Flat-solid discs read as wireframes; this reads
// as a finished mark without straying into skeuomorphic territory.
const fallbackBackground = computed(() =>
  `radial-gradient(circle at 32% 26%, ` +
    `color-mix(in oklab, ${bgColor.value}, white 22%), ` +
    `${bgColor.value} 58%, ` +
    `color-mix(in oklab, ${bgColor.value}, black 14%))`,
);

const showImage = computed(() => !!props.src && !imageFailed.value);

const presenceDotColor = computed(() => {
  switch (props.presence) {
    case "online": return "bg-success/75";
    case "away":   return "bg-warning/75";
    case "dnd":    return "bg-destructive/75";
    case "offline": return "bg-muted-foreground/25";
    default:       return null;
  }
});

const dotSize = computed(() => {
  if (props.size === "xs") return "w-1.5 h-1.5 border";
  if (props.size === "message") return "w-2 h-2 border-[1.5px]";
  if (props.size === "lg") return "w-2 h-2 border-[1.5px]";
  return "w-2 h-2 border-[1.5px]";
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

watch(
  () => props.src,
  () => {
    imageFailed.value = false;
  },
);
</script>

<template>
  <div
    class="app-avatar relative flex-shrink-0 leading-none"
    :class="[
      wrapperSizeClass,
      presence === 'offline' ? 'app-avatar--offline' : '',
    ]"
    :title="presenceTooltip"
  >
    <img
      v-if="showImage"
      :src="props.src ?? undefined"
      :alt="name"
      :class="[sizeClass, 'block rounded-lg object-cover bg-muted']"
      loading="lazy"
      @error="imageFailed = true"
    />
    <div
      v-else
      :class="[sizeClass, 'type-avatar-mark flex items-center justify-center rounded-lg text-white']"
      :style="{
        background: fallbackBackground,
        boxShadow: `inset 0 1px 0 rgba(255,255,255,0.18), 0 2px 8px ${bgColor}30`,
      }"
    >
      {{ initials }}
    </div>
    <span
      v-if="presenceDotColor"
      class="app-avatar-presence-dot absolute rounded-full border-background"
      :class="[dotSize, presenceDotColor]"
    />
    <span
      v-if="inCall"
      class="app-avatar-call-badge absolute -top-1 -right-1 flex items-center justify-center rounded-full border border-background bg-success text-white"
      :class="dotSize"
      role="img"
      aria-label="In a call"
      title="In a call"
    >
      <svg viewBox="0 0 24 24" fill="currentColor" class="w-2 h-2" aria-hidden="true">
        <path d="M6.6 10.8a15.2 15.2 0 0 0 6.6 6.6l2.2-2.2a1 1 0 0 1 1-.25 11.4 11.4 0 0 0 3.6.57 1 1 0 0 1 1 1V20a1 1 0 0 1-1 1A17 17 0 0 1 3 4a1 1 0 0 1 1-1h3.5a1 1 0 0 1 1 1c0 1.25.2 2.46.57 3.6a1 1 0 0 1-.25 1z" />
      </svg>
    </span>
  </div>
</template>

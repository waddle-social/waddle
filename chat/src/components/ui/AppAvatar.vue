<script setup lang="ts">
import { computed } from "vue";
import { Avatar } from "@ark-ui/vue/avatar";
import { avatar as avatarRecipe } from "styled-system/recipes";
import { token } from "styled-system/tokens";
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
  /** In a huddle: a teal ring around the avatar. */
  huddle?: boolean;
  /** Speaking in a huddle: the ring glows. Implies `huddle`. */
  speaking?: boolean;
}>();

// One literal recipe call per variant value so Panda's static extraction
// emits every size and ring class the component can reach at runtime.
const SIZE_CLASSES = {
  sm: avatarRecipe({ size: "sm" }),
  md: avatarRecipe({ size: "md" }),
  lg: avatarRecipe({ size: "lg" }),
  xl: avatarRecipe({ size: "xl" }),
} as const;
const HUDDLE_ROOT = variantClasses(avatarRecipe({ huddle: true }).root, "--huddle_");
const SPEAKING_ROOT = variantClasses(avatarRecipe({ speaking: true }).root, "--speaking_");

/** Keep only the classes of one variant so it can be layered on any size. */
function variantClasses(classList: string, marker: string): string {
  return classList.split(" ").filter((c) => c.includes(marker)).join(" ");
}

const TINTS = [
  token("colors.avatarTints.1"),
  token("colors.avatarTints.2"),
  token("colors.avatarTints.3"),
  token("colors.avatarTints.4"),
];

const initials = computed(() =>
  props.name
    .split(" ")
    .map((n) => n[0])
    .join("")
    .toUpperCase()
    .slice(0, 2),
);

// `xs` and `message` keep their exact legacy dimensions (the message grid
// column is sized from `--chat-message-avatar-size`); the other sizes come
// straight from the recipe.
const recipeSize = computed<keyof typeof SIZE_CLASSES>(() => {
  switch (props.size) {
    case "xs":
    case "sm":
      return "sm";
    case "lg":
      return "lg";
    case "xl":
      return "xl";
    default:
      return "md";
  }
});

const cls = computed(() => SIZE_CLASSES[recipeSize.value]);

const fixedSize = computed(() => {
  if (props.size === "xs") return "1.5rem";
  if (props.size === "message") return "var(--chat-message-avatar-size)";
  return null;
});

const rootStyle = computed(() =>
  fixedSize.value ? { width: fixedSize.value, height: fixedSize.value } : undefined,
);

const rootClass = computed(() => [
  "app-avatar",
  cls.value.root,
  props.speaking ? SPEAKING_ROOT : props.huddle ? HUDDLE_ROOT : "",
  props.presence === "offline" ? "app-avatar--offline" : "",
]);

// Name-derived tint so a person keeps the same fallback colour everywhere.
const tint = computed(() => {
  let hash = 5381;
  for (let i = 0; i < props.name.length; i++) hash = ((hash << 5) + hash + props.name.charCodeAt(i)) | 0;
  return TINTS[Math.abs(hash) % TINTS.length];
});

const presenceShow = computed(() => {
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
    <Avatar.Root :class="rootClass" :style="rootStyle">
      <Avatar.Image
        v-if="src"
        :src="src"
        :alt="name"
        :class="[cls.image, 'bg-muted']"
        loading="lazy"
      />
      <Avatar.Fallback
        :class="[cls.fallback, 'type-avatar-mark select-none']"
        :style="{ background: tint }"
      >
        {{ initials }}
      </Avatar.Fallback>
      <span
        v-if="presenceShow"
        :class="cls.presence"
        :data-show="presenceShow"
        aria-hidden="true"
      />
      <span
        v-if="inCall"
        class="app-avatar-call-badge absolute -top-1 -right-1 flex h-3 w-3 items-center justify-center rounded-full border-[1.5px] border-background bg-success text-white"
        role="img"
        aria-label="In a call"
      >
        <svg viewBox="0 0 24 24" fill="currentColor" class="w-2 h-2" aria-hidden="true">
          <path d="M6.6 10.8a15.2 15.2 0 0 0 6.6 6.6l2.2-2.2a1 1 0 0 1 1-.25 11.4 11.4 0 0 0 3.6.57 1 1 0 0 1 1 1V20a1 1 0 0 1-1 1A17 17 0 0 1 3 4a1 1 0 0 1 1-1h3.5a1 1 0 0 1 1 1c0 1.25.2 2.46.57 3.6a1 1 0 0 1-.25 1z" />
        </svg>
      </span>
    </Avatar.Root>
  </AppTooltip>
</template>

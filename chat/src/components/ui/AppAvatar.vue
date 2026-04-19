<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { consistentColor } from "@/lib/chat-ui";
import type { OccupantPresence } from "@/lib/xmpp-client";

const props = defineProps<{
  name: string;
  src?: string | null;
  size?: "xs" | "sm" | "md" | "lg";
  presence?: OccupantPresence;
  lastSeen?: number;
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
  if (props.size === "xs") return "w-6 h-6 text-[10px]";
  if (props.size === "sm") return "w-7 h-7 text-[11px]";
  if (props.size === "lg") return "w-12 h-12 text-base";
  return "w-8 h-8 text-xs";
});

const wrapperSizeClass = computed(() => {
  if (props.size === "xs") return "w-6 h-6";
  if (props.size === "sm") return "w-7 h-7";
  if (props.size === "lg") return "w-12 h-12";
  return "w-8 h-8";
});

const bgColor = computed(() => consistentColor(props.name, 55, 45));
const showImage = computed(() => !!props.src && !imageFailed.value);

const presenceDotColor = computed(() => {
  switch (props.presence) {
    case "online": return "bg-success shadow-[0_0_4px_var(--success)]";
    case "away":   return "bg-warning shadow-[0_0_4px_var(--warning)]";
    case "dnd":    return "bg-destructive shadow-[0_0_4px_var(--destructive)]";
    case "offline": return "bg-muted-foreground/30";
    default:       return null;
  }
});

const dotSize = computed(() => {
  if (props.size === "xs") return "w-[7px] h-[7px] border-[1.5px]";
  if (props.size === "lg") return "w-2.5 h-2.5 border-2";
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
    class="relative flex-shrink-0"
    :class="[wrapperSizeClass, presence === 'offline' ? 'opacity-50' : '']"
    :title="presenceTooltip"
  >
    <img
      v-if="showImage"
      :src="props.src ?? undefined"
      :alt="name"
      :class="[sizeClass, 'rounded-lg object-cover bg-muted']"
      loading="lazy"
      @error="imageFailed = true"
    />
    <div
      v-else
      :class="[sizeClass, 'flex items-center justify-center font-semibold text-white rounded-lg']"
      :style="{ backgroundColor: bgColor, boxShadow: `0 2px 8px ${bgColor}30` }"
    >
      {{ initials }}
    </div>
    <span
      v-if="presenceDotColor"
      class="absolute bottom-0 right-0 translate-x-[2px] translate-y-[2px] rounded-full border-background"
      :class="[dotSize, presenceDotColor]"
    />
  </div>
</template>

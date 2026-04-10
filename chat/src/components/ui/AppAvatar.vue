<script setup lang="ts">
import { computed } from "vue";
import { consistentColor } from "@/lib/chat-ui";

const props = defineProps<{
  name: string;
  size?: "sm" | "md";
}>();

const initials = computed(() =>
  props.name
    .split(" ")
    .map((n) => n[0])
    .join("")
    .toUpperCase()
    .slice(0, 2),
);

const sizeClass = computed(() =>
  props.size === "sm" ? "w-8 h-8 text-xs" : "w-10 h-10 text-sm",
);

const bgColor = computed(() => consistentColor(props.name, 65, 45));
</script>

<template>
  <div
    :class="[sizeClass, 'flex items-center justify-center font-mono font-bold flex-shrink-0 text-white']"
    :style="{ backgroundColor: bgColor }"
  >
    {{ initials }}
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { consistentColor } from "@/lib/chat-ui";

const props = defineProps<{
  name: string;
  size?: "xs" | "sm" | "md";
}>();

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
  return "w-8 h-8 text-xs";
});

const bgColor = computed(() => consistentColor(props.name, 55, 45));
</script>

<template>
  <div
    :class="[sizeClass, 'flex items-center justify-center font-semibold flex-shrink-0 text-white rounded-lg']"
    :style="{ backgroundColor: bgColor, boxShadow: `0 2px 8px ${bgColor}30` }"
  >
    {{ initials }}
  </div>
</template>

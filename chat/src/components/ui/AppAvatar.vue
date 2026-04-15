<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { consistentColor } from "@/lib/chat-ui";

const props = defineProps<{
  name: string;
  src?: string | null;
  size?: "xs" | "sm" | "md";
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
  return "w-8 h-8 text-xs";
});

const bgColor = computed(() => consistentColor(props.name, 55, 45));
const showImage = computed(() => !!props.src && !imageFailed.value);

watch(
  () => props.src,
  () => {
    imageFailed.value = false;
  },
);
</script>

<template>
  <img
    v-if="showImage"
    :src="props.src ?? undefined"
    :alt="name"
    :class="[sizeClass, 'flex-shrink-0 rounded-lg object-cover bg-muted']"
    loading="lazy"
    @error="imageFailed = true"
  />
  <div
    v-else
    :class="[sizeClass, 'flex items-center justify-center font-semibold flex-shrink-0 text-white rounded-lg']"
    :style="{ backgroundColor: bgColor, boxShadow: `0 2px 8px ${bgColor}30` }"
  >
    {{ initials }}
  </div>
</template>

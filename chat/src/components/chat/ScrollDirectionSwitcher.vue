<script setup lang="ts">
import { useScrollDirection } from "@/composables/useScrollDirection";
import type { ScrollDirectionMode } from "@/lib/scroll-direction";

const { mode, setScrollDirection } = useScrollDirection();

const options: ReadonlyArray<{
  value: ScrollDirectionMode;
  label: string;
  description: string;
}> = [
  { value: "chat", label: "Newest at bottom", description: "Keep the classic chat flow with fresh posts landing below older ones." },
  { value: "social", label: "Newest at top", description: "Flip the timeline so the latest posts stay first as you read downward." },
];
</script>

<template>
  <div class="flex flex-col gap-2">
    <div class="flex items-center justify-between gap-3">
      <span class="text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/70">
        Scroll direction
      </span>
    </div>
    <div
      role="radiogroup"
      aria-label="Scroll direction"
      class="grid grid-cols-2 gap-1 rounded-lg border border-border bg-muted/30 p-1"
    >
      <button
        v-for="option in options"
        :key="option.value"
        type="button"
        role="radio"
        :aria-checked="mode === option.value"
        class="min-h-16 rounded-md px-3 py-2 text-left transition-colors duration-150"
        :class="mode === option.value
          ? 'bg-background text-foreground shadow-sm'
          : 'text-muted-foreground hover:text-foreground'"
        @click="setScrollDirection(option.value)"
      >
        <div class="text-[12px] font-medium">{{ option.label }}</div>
        <div class="mt-0.5 text-[11px] leading-snug text-muted-foreground">
          {{ option.description }}
        </div>
      </button>
    </div>
  </div>
</template>

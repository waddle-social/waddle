<script setup lang="ts">
import { button } from "styled-system/recipes";

const actionButtonClass = button({ variant: "primary", size: "lg" });

defineProps<{
  title: string;
  copy?: string;
  actionLabel?: string;
}>();

defineEmits<{
  action: [];
}>();
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-6 bg-background">
    <div class="w-full max-w-sm animate-slide-up">
      <div class="flex flex-col gap-5 rounded-2xl border border-border bg-card p-6">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl border border-border bg-background">
          <img
            src="/waddle-logo.svg"
            alt="Waddle"
            width="40"
            height="40"
            class="h-10 w-10 object-contain"
          />
        </div>
        <div class="chat-field-stack">
          <h1 class="font-display text-[30px] font-bold leading-none tracking-[-0.03em] text-foreground">
            {{ title }}
          </h1>
          <p v-if="copy" class="text-sm text-muted-foreground">
            {{ copy }}
          </p>
        </div>
        <button
          v-if="actionLabel"
          :class="[actionButtonClass, 'w-full']"
          type="button"
          @click="$emit('action')"
        >
          {{ actionLabel }}
        </button>
      </div>
    </div>
  </div>
</template>

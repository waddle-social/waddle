<script setup lang="ts">
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
      <div class="glass-panel flex flex-col gap-4 rounded-lg border border-border p-6 shadow-2xl">
        <div class="w-12 h-12 rounded-lg bg-primary/10 flex items-center justify-center shadow-[0_0_20px_var(--glow)]">
          <img
            src="/waddle-logo.svg"
            alt="Waddle"
            width="40"
            height="40"
            class="h-10 w-10 object-contain"
          />
        </div>
        <div class="chat-field-stack">
          <h1 class="type-display-title">
            {{ title }}
          </h1>
          <p v-if="copy" class="type-field text-muted-foreground">
            {{ copy }}
          </p>
        </div>
        <button
          v-if="actionLabel"
          class="chat-action-button chat-action-button--primary type-action w-full"
          type="button"
          @click="$emit('action')"
        >
          {{ actionLabel }}
        </button>
      </div>
    </div>
  </div>
</template>

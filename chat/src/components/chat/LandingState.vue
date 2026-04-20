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
      <div class="glass-panel rounded-2xl border border-border p-7 shadow-2xl">
        <div class="w-12 h-12 rounded-xl bg-primary/10 flex items-center justify-center mb-5 shadow-[0_0_20px_var(--glow)]">
          <img
            src="/waddle-logo.svg"
            alt="Waddle"
            width="40"
            height="40"
            class="h-10 w-10 object-contain"
          />
        </div>
        <h1 class="text-[20px] font-display font-bold tracking-tight mb-2">
          {{ title }}
        </h1>
        <p v-if="copy" class="text-[13px] text-muted-foreground leading-relaxed mb-5">
          {{ copy }}
        </p>
        <button
          v-if="actionLabel"
          class="w-full py-2.5 px-4 text-[13px] font-semibold rounded-xl bg-primary text-primary-foreground hover:shadow-[0_0_20px_var(--glow-strong)] transition-all duration-300"
          @click="$emit('action')"
        >
          {{ actionLabel }}
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue';

type Variant = 'default' | 'outline' | 'secondary';

const props = withDefaults(
  defineProps<{
    variant?: Variant;
  }>(),
  {
    variant: 'default',
  },
);

const variantClasses: Record<Variant, string> = {
  default: 'bg-foreground text-background',
  outline: 'border border-foreground text-foreground',
  secondary: 'bg-muted text-foreground',
};

const classes = computed(() =>
  ['inline-flex items-center rounded-none px-2 py-0.5 text-xs font-semibold uppercase tracking-wide gap-1', variantClasses[props.variant]].join(' '),
);
</script>

<template>
  <span :class="classes" v-bind="$attrs">
    <slot />
  </span>
</template>

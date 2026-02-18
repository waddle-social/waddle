<script setup lang="ts">
import { computed } from 'vue';

type Variant = 'default' | 'ghost' | 'outline';
type Size = 'default' | 'icon' | 'sm';

type ButtonType = 'button' | 'submit' | 'reset';

const props = withDefaults(
  defineProps<{
    variant?: Variant;
    size?: Size;
    type?: ButtonType;
  }>(),
  {
    variant: 'default',
    size: 'default',
    type: 'button',
  },
);

const variantClasses: Record<Variant, string> = {
  default: 'bg-foreground text-background hover:bg-foreground/90',
  ghost: 'bg-transparent hover:bg-foreground/10',
  outline: 'border border-foreground/30 hover:bg-foreground/5',
};

const sizeClasses: Record<Size, string> = {
  default: 'h-10 px-4 text-sm',
  icon: 'h-8 w-8',
  sm: 'h-8 px-3 text-xs',
};

const classes = computed(() =>
  [
    'inline-flex items-center justify-center rounded-none font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/60 focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:pointer-events-none disabled:opacity-60 gap-2',
    variantClasses[props.variant],
    sizeClasses[props.size],
  ].join(' '),
);
</script>

<template>
  <button :type="props.type" :class="classes" v-bind="$attrs">
    <slot />
  </button>
</template>

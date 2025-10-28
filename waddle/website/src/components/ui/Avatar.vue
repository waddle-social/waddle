<script setup lang="ts">
import { ref } from 'vue';

const props = withDefaults(
  defineProps<{
    src?: string;
    alt?: string;
    imgClasses?: string;
  }>(),
  {
    alt: 'Avatar',
    imgClasses: 'h-full w-full object-cover',
  },
);

const isErrored = ref(false);
const handleError = () => {
  isErrored.value = true;
};
</script>

<template>
  <div class="relative flex h-10 w-10 items-center justify-center overflow-hidden bg-muted text-muted-foreground" v-bind="$attrs">
    <img
      v-if="props.src && !isErrored"
      :src="props.src"
      :alt="props.alt"
      :class="props.imgClasses"
      @error="handleError"
      loading="lazy"
      decoding="async"
    />
    <slot v-else />
  </div>
</template>

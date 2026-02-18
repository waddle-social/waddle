<script setup lang="ts">
import { computed, ref } from 'vue';

type InputType = string;

const props = withDefaults(
  defineProps<{
    type?: InputType;
  }>(),
  {
    type: 'text',
  },
);

const classes = computed(
  () =>
    'h-9 w-full rounded-none border border-border bg-transparent px-3 text-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/60 focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:cursor-not-allowed disabled:opacity-60',
);

const inputRef = ref<HTMLInputElement | null>(null);

defineExpose({
  focus: () => inputRef.value?.focus(),
  el: inputRef,
});
</script>

<template>
  <input ref="inputRef" :type="props.type" :class="classes" v-bind="$attrs" />
</template>

<template>
  <div class="flex flex-wrap gap-2 p-4 bg-black/30 backdrop-blur-xl border-b border-white/20 ring-1 ring-white/5">
    <!-- Show All Button -->
    <button
      @click="send({ type: 'SHOW_ALL' })"
      :class="[
        'px-3 py-1.5 rounded-full text-xs font-medium transition-all duration-200 border',
        snapshot?.context?.showAll
          ? 'bg-white/20 text-white border-white/30'
          : 'bg-white/5 text-white/60 border-white/10 hover:bg-white/10 hover:text-white/80'
      ]"
    >
      All
    </button>

    <!-- Category Filter Buttons -->
    <button
      v-for="category in CATEGORIES"
      :key="category"
      @click="send({ type: 'TOGGLE_FILTER', category })"
      :class="[
        'px-3 py-1.5 rounded-full text-xs font-medium transition-all duration-200 border',
        snapshot?.context?.activeFilters?.has(category)
          ? 'bg-accent-primary text-white border-accent-primary/50 shadow-lg transform scale-105'
          : 'bg-white/5 text-white/60 border-white/10 hover:bg-white/10 hover:text-white/80 hover:scale-102'
      ]"
    >
      {{ category }}
    </button>

    <!-- Clear All Button -->
    <button
      v-if="!snapshot?.context?.showAll"
      @click="send({ type: 'CLEAR_ALL' })"
      class="px-3 py-1.5 rounded-full text-xs font-medium transition-all duration-200 bg-red-500/20 text-red-300 border border-red-400/30 hover:bg-red-500/30 hover:text-red-200"
    >
      Clear
    </button>
  </div>
</template>

<script setup lang="ts">
import { useMachine } from '@xstate/vue'
import { filterMachine, CATEGORIES } from '../machines/filterMachine'

const { snapshot, send } = useMachine(filterMachine)

defineExpose({
  current: snapshot,
  send
})
</script>
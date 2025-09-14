<template>
  <div :class="containerClass">
    <!-- Chat Room Loading Skeleton -->
    <div v-if="type === 'chatroom'" class="h-screen flex flex-col bg-gradient-to-br from-black to-gray-900">
      <!-- Header Skeleton -->
      <div class="p-4 bg-black/20 backdrop-blur-xl border-b border-white/10">
        <div class="flex items-center justify-between">
          <div class="flex items-center gap-4">
            <div class="h-8 w-32 bg-white/10 rounded animate-pulse"></div>
            <div class="h-4 w-16 bg-white/5 rounded animate-pulse"></div>
          </div>
          <div class="flex items-center gap-4">
            <div class="h-6 w-20 bg-white/5 rounded animate-pulse"></div>
            <div class="h-8 w-8 bg-white/10 rounded-full animate-pulse"></div>
            <div class="h-8 w-16 bg-white/5 rounded animate-pulse"></div>
          </div>
        </div>
      </div>

      <!-- Filter Bar Skeleton -->
      <div class="flex gap-2 p-4 bg-black/20 border-b border-white/10">
        <div class="h-6 w-12 bg-white/10 rounded-full animate-pulse"></div>
        <div class="h-6 w-16 bg-white/5 rounded-full animate-pulse"></div>
        <div class="h-6 w-14 bg-white/5 rounded-full animate-pulse"></div>
        <div class="h-6 w-18 bg-white/5 rounded-full animate-pulse"></div>
        <div class="h-6 w-12 bg-white/5 rounded-full animate-pulse"></div>
      </div>

      <!-- Messages Skeleton -->
      <div class="flex-1 p-4 space-y-4 overflow-hidden">
        <div v-for="i in 6" :key="i" :class="[
          'p-4 rounded-2xl border backdrop-blur-sm',
          i % 3 === 0 ? 'bg-black/40 border-white/20 ml-8' : 'bg-black/20 border-white/10 mr-8'
        ]">
          <div class="flex items-center gap-3 mb-3">
            <div class="w-8 h-8 bg-white/10 rounded-full animate-pulse"></div>
            <div class="h-4 w-20 bg-white/10 rounded animate-pulse"></div>
            <div class="ml-auto h-3 w-12 bg-white/5 rounded animate-pulse"></div>
          </div>
          <div class="space-y-2">
            <div class="h-4 bg-white/10 rounded animate-pulse" :style="{ width: `${60 + Math.random() * 30}%` }"></div>
            <div v-if="Math.random() > 0.5" class="h-4 bg-white/10 rounded animate-pulse" :style="{ width: `${40 + Math.random() * 40}%` }"></div>
          </div>
        </div>
      </div>

      <!-- Input Skeleton -->
      <div class="p-4 bg-black/20 border-t border-white/10">
        <div class="flex gap-2 mb-3">
          <div class="h-6 w-12 bg-white/5 rounded-full animate-pulse"></div>
          <div class="h-6 w-16 bg-white/5 rounded-full animate-pulse"></div>
          <div class="h-6 w-14 bg-white/5 rounded-full animate-pulse"></div>
        </div>
        <div class="flex gap-3">
          <div class="flex-1 h-12 bg-white/5 rounded-xl animate-pulse"></div>
          <div class="h-12 w-16 bg-accent-primary/20 rounded-xl animate-pulse"></div>
        </div>
      </div>
    </div>

    <!-- Login Form Loading Skeleton -->
    <div v-else-if="type === 'login'" class="min-h-screen flex items-center justify-center bg-gradient-to-br from-black to-gray-900">
      <div class="w-full max-w-md p-8 m-4">
        <div class="backdrop-blur-2xl bg-black/40 border border-white/20 rounded-3xl p-8">
          <div class="text-center mb-8">
            <div class="h-10 w-32 bg-white/10 rounded mx-auto mb-2 animate-pulse"></div>
            <div class="h-4 w-48 bg-white/5 rounded mx-auto animate-pulse"></div>
          </div>
          <div class="space-y-6">
            <div>
              <div class="h-4 w-16 bg-white/10 rounded mb-2 animate-pulse"></div>
              <div class="h-12 bg-white/5 rounded-xl animate-pulse"></div>
            </div>
            <div class="h-12 bg-accent-primary/20 rounded-xl animate-pulse"></div>
          </div>
          <div class="mt-8 text-center">
            <div class="h-3 w-40 bg-white/5 rounded mx-auto animate-pulse"></div>
          </div>
        </div>
      </div>
    </div>

    <!-- Message List Loading Skeleton -->
    <div v-else-if="type === 'messages'" class="space-y-4 p-4">
      <div v-for="i in count" :key="i" :class="[
        'p-4 rounded-2xl border backdrop-blur-sm animate-pulse',
        i % 3 === 0 ? 'bg-black/40 border-white/20 ml-8' : 'bg-black/20 border-white/10 mr-8'
      ]">
        <div class="flex items-center gap-3 mb-3">
          <div class="w-8 h-8 bg-white/10 rounded-full"></div>
          <div class="h-4 w-20 bg-white/10 rounded"></div>
          <div class="ml-auto h-3 w-12 bg-white/5 rounded"></div>
        </div>
        <div class="space-y-2">
          <div class="h-4 bg-white/10 rounded" :style="{ width: `${60 + Math.random() * 30}%` }"></div>
          <div v-if="Math.random() > 0.5" class="h-4 bg-white/10 rounded" :style="{ width: `${40 + Math.random() * 40}%` }"></div>
        </div>
      </div>
    </div>

    <!-- Generic Loading Skeleton -->
    <div v-else class="space-y-4 p-4">
      <div v-for="i in count" :key="i" class="animate-pulse">
        <div class="h-4 bg-white/10 rounded mb-2" :style="{ width: `${70 + Math.random() * 20}%` }"></div>
        <div v-if="Math.random() > 0.3" class="h-4 bg-white/5 rounded" :style="{ width: `${50 + Math.random() * 30}%` }"></div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

interface Props {
  type?: 'chatroom' | 'login' | 'messages' | 'generic'
  count?: number
  class?: string
}

const props = withDefaults(defineProps<Props>(), {
  type: 'generic',
  count: 3,
  class: '',
})

const containerClass = computed(() => {
  return [
    props.class,
    'animate-pulse',
  ].filter(Boolean).join(' ')
})
</script>

<style scoped>
@keyframes pulse {
  0%, 100% {
    opacity: 1;
  }
  50% {
    opacity: 0.5;
  }
}

.animate-pulse {
  animation: pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite;
}

/* Stagger animation for multiple items */
.animate-pulse:nth-child(1) { animation-delay: 0ms; }
.animate-pulse:nth-child(2) { animation-delay: 150ms; }
.animate-pulse:nth-child(3) { animation-delay: 300ms; }
.animate-pulse:nth-child(4) { animation-delay: 450ms; }
.animate-pulse:nth-child(5) { animation-delay: 600ms; }
.animate-pulse:nth-child(6) { animation-delay: 750ms; }
</style>
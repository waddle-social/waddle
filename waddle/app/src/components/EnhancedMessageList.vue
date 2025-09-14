<template>
  <div ref="messagesContainer" class="flex-1 overflow-y-auto p-4 space-y-3">
    <!-- Welcome message when no messages -->
    <div v-if="filteredMessages.length === 0" class="flex items-center justify-center h-full">
      <div class="text-center text-white/50">
        <div class="text-6xl mb-4">💬</div>
        <h3 class="text-xl font-semibold mb-2">Welcome to Enhanced Waddle</h3>
        <p class="text-sm">Start the conversation! Each message is powered by its own XState actor.</p>
      </div>
    </div>

    <!-- Messages -->
    <div
      v-for="message in filteredMessages"
      :key="message.id"
      :class="[
        'p-4 rounded-2xl backdrop-blur-lg border transition-all duration-200 hover:scale-[1.01] relative ring-1 ring-white/5',
        message.username === currentUsername 
          ? 'bg-black/40 border-white/30 ml-8' 
          : 'bg-black/20 border-white/15 mr-8',
        pendingMessages.has(message.id) && 'animate-pulse',
        failedMessages.has(message.id) && 'border-red-400/30 bg-red-500/10'
      ]"
    >
      <!-- Message Status Indicator -->
      <div class="absolute top-2 right-2">
        <div v-if="pendingMessages.has(message.id)" class="flex items-center gap-1">
          <div class="w-2 h-2 bg-yellow-400 rounded-full animate-pulse"></div>
          <span class="text-xs text-yellow-400">Sending...</span>
        </div>
        <div v-else-if="failedMessages.has(message.id)" class="flex items-center gap-1">
          <div class="w-2 h-2 bg-red-400 rounded-full"></div>
          <span class="text-xs text-red-400">Failed</span>
        </div>
        <div v-else-if="message.username === currentUsername" class="flex items-center gap-1">
          <div class="w-2 h-2 bg-green-400 rounded-full"></div>
          <span class="text-xs text-green-400">Sent</span>
        </div>
      </div>

      <!-- Message Header -->
      <div class="flex items-center justify-between mb-2 pr-20">
        <div class="flex items-center gap-3">
          <!-- Avatar -->
          <div :class="[
            'w-8 h-8 rounded-full flex items-center justify-center text-xs font-bold',
            message.username === currentUsername
              ? 'bg-accent-primary text-white'
              : 'bg-accent-secondary text-white'
          ]">
            {{ message.username.charAt(0).toUpperCase() }}
          </div>
          
          <!-- Username -->
          <span :class="[
            'font-semibold text-sm',
            message.username === currentUsername ? 'text-white' : 'text-white'
          ]">
            {{ message.username }}
          </span>
        </div>
        
        <div class="flex items-center gap-2">
          <!-- Category Badge -->
          <span class="px-2 py-1 rounded-full text-xs bg-white/10 text-white/70">
            {{ message.category }}
          </span>
          
          <!-- Timestamp -->
          <span class="text-xs text-white/40">
            {{ formatTime(message.timestamp) }}
          </span>
        </div>
      </div>

      <!-- Message Content -->
      <div :class="[
        'text-sm leading-relaxed break-words',
        message.username === currentUsername ? 'text-white' : 'text-white/90'
      ]">
        {{ message.content }}
      </div>

      <!-- Message Actions (for failed messages) -->
      <div v-if="failedMessages.has(message.id)" class="flex items-center gap-2 mt-3 pt-3 border-t border-white/10">
        <button
          @click="$emit('retryMessage', message.id)"
          class="px-3 py-1 bg-yellow-500/20 hover:bg-yellow-500/30 text-yellow-300 rounded-lg text-xs transition-all duration-200"
        >
          Retry
        </button>
        <button
          @click="$emit('deleteMessage', message.id)"
          class="px-3 py-1 bg-red-500/20 hover:bg-red-500/30 text-red-300 rounded-lg text-xs transition-all duration-200"
        >
          Delete
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, watch, ref } from 'vue'
import type { MessageData } from '../machines/actors/messageActor'
import type { Category } from '../machines/filterMachine'

interface Props {
  messages: MessageData[]
  pendingMessages: Set<string>
  failedMessages: Set<string>
  currentUsername: string
  activeFilters: Set<Category>
  showAll: boolean
}

const props = defineProps<Props>()

const emit = defineEmits<{
  retryMessage: [messageId: string]
  deleteMessage: [messageId: string]
}>()

const messagesContainer = ref<HTMLElement>()

const filteredMessages = computed(() => {
  if (props.showAll) {
    return props.messages
  }
  
  if (props.activeFilters.size === 0) {
    return props.messages
  }
  
  return props.messages.filter(message => 
    props.activeFilters.has(message.category as Category)
  )
})

const formatTime = (timestamp: number) => {
  const date = new Date(timestamp)
  const now = new Date()
  const diff = now.getTime() - date.getTime()
  const minutes = Math.floor(diff / 60000)
  
  if (minutes < 1) return 'now'
  if (minutes < 60) return `${minutes}m ago`
  
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  
  return date.toLocaleDateString()
}

// Auto-scroll to bottom when new messages arrive
watch(() => props.messages.length, async () => {
  await nextTick()
  if (messagesContainer.value) {
    messagesContainer.value.scrollTop = messagesContainer.value.scrollHeight
  }
})

// Auto-scroll when filter changes
watch([() => props.activeFilters, () => props.showAll], async () => {
  await nextTick()
  if (messagesContainer.value) {
    messagesContainer.value.scrollTop = messagesContainer.value.scrollHeight
  }
})
</script>
<template>
  <!-- Loading state while XState initializes -->
  <div v-if="!snapshot" class="min-h-screen flex items-center justify-center bg-gradient-to-br from-black to-gray-900">
    <div class="text-white text-xl">Loading...</div>
  </div>
  
  <!-- Main component -->
  <div v-else class="h-screen flex flex-col bg-gradient-to-br from-black to-gray-900 relative overflow-hidden">
    <!-- Animated background -->
    <div class="absolute inset-0 bg-gradient-to-r from-gray-800/5 via-slate-800/5 to-gray-800/5"></div>
    <div class="absolute top-1/3 left-1/3 w-96 h-96 bg-white/3 rounded-full blur-3xl"></div>
    <div class="absolute bottom-1/3 right-1/3 w-96 h-96 bg-white/2 rounded-full blur-3xl"></div>

    <!-- Header -->
    <header class="relative z-10 p-4 bg-black/30 backdrop-blur-2xl border-b border-white/20 ring-1 ring-white/5">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-4">
          <h1 class="text-2xl font-bold text-white">
            Waddle Chat (Enhanced)
          </h1>
          
          <!-- Connection Status -->
          <div class="flex items-center gap-2">
            <div :class="[
              'w-2 h-2 rounded-full',
              snapshot?.context?.connectionStatus === 'connected' 
                ? 'bg-green-400 animate-pulse' 
                : snapshot?.context?.connectionStatus === 'connecting'
                ? 'bg-yellow-400 animate-pulse'
                : 'bg-red-400'
            ]"></div>
            <span class="text-xs text-white/60 capitalize">
              {{ snapshot?.context?.connectionStatus }}
            </span>
          </div>
          
          <!-- Message Stats -->
          <div class="flex items-center gap-4 text-xs text-white/50">
            <span>{{ Array.from(snapshot?.context?.messages?.values() || []).length }} messages</span>
            <span v-if="snapshot?.context?.pendingMessages?.size">{{ snapshot.context.pendingMessages.size }} sending</span>
            <span v-if="snapshot?.context?.failedMessages?.size" class="text-red-400">{{ snapshot.context.failedMessages.size }} failed</span>
          </div>
        </div>

        <div class="flex items-center gap-4">
          <!-- Online Users Count (placeholder) -->
          <div class="flex items-center gap-2 text-sm text-white/60">
            <div class="w-4 h-4 rounded-full bg-green-400/20 flex items-center justify-center">
              <div class="w-2 h-2 rounded-full bg-green-400"></div>
            </div>
            <span>{{ onlineCount }} online</span>
          </div>

          <!-- Username -->
          <div class="flex items-center gap-2">
            <div class="w-8 h-8 rounded-full bg-accent-primary flex items-center justify-center text-sm font-bold text-white">
              {{ username.charAt(0).toUpperCase() }}
            </div>
            <span class="text-white font-medium">{{ username }}</span>
          </div>

          <!-- Logout Button -->
          <button
            @click="handleLogout"
            class="px-3 py-1.5 bg-white/10 hover:bg-white/20 text-white/80 hover:text-white rounded-lg transition-all duration-200 text-sm border border-white/20"
          >
            Logout
          </button>
        </div>
      </div>
    </header>

    <!-- Filter Bar -->
    <FilterBar 
      ref="filterBar"
      class="relative z-10"
    />

    <!-- Messages Area -->
    <div class="flex-1 relative z-10 flex flex-col min-h-0">
      <EnhancedMessageList
        :messages="Array.from(snapshot?.context?.messages?.values() || [])"
        :pending-messages="snapshot?.context?.pendingMessages || new Set()"
        :failed-messages="snapshot?.context?.failedMessages || new Set()"
        :current-username="username"
        :active-filters="filterBar?.current?.context?.activeFilters || new Set()"
        :show-all="filterBar?.current?.context?.showAll || true"
        @retry-message="handleRetryMessage"
        @delete-message="handleDeleteMessage"
      />
    </div>

    <!-- Message Input -->
    <MessageInput
      ref="messageInput"
      @send-message="handleSendMessage"
      class="relative z-10"
    />

    <!-- Connection Error -->
    <div
      v-if="snapshot?.context?.error"
      class="absolute top-20 left-1/2 transform -translate-x-1/2 z-20 bg-red-500/90 text-white px-4 py-2 rounded-lg backdrop-blur-sm"
    >
      {{ snapshot.context.error }}
    </div>

    <!-- Subtle grid overlay -->
    <div class="absolute inset-0 opacity-20 bg-[url('/grid-pattern.svg')]"></div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { useMachine } from '@xstate/vue'
import { enhancedChatMachine } from '../machines/enhancedChatMachine'
import FilterBar from './FilterBar.vue'
import EnhancedMessageList from './EnhancedMessageList.vue'
import MessageInput from './MessageInput.vue'
import type { MessageData } from '../machines/actors/messageActor'

interface Props {
  username: string
}

const props = defineProps<Props>()

const { snapshot, send } = useMachine(enhancedChatMachine)
const filterBar = ref<InstanceType<typeof FilterBar>>()
const messageInput = ref<InstanceType<typeof MessageInput>>()
const onlineCount = ref(1) // Placeholder for real user count

const handleSendMessage = ({ content, category }: { content: string; category: string }) => {
  send({ type: 'SEND_MESSAGE', content, category })
  messageInput.value?.send({ type: 'SEND_SUCCESS' })
}

const handleRetryMessage = (messageId: string) => {
  send({ type: 'RETRY_MESSAGE', messageId })
}

const handleDeleteMessage = (messageId: string) => {
  send({ type: 'DELETE_MESSAGE', messageId })
}

const handleLogout = () => {
  send({ type: 'DISCONNECT' })
  sessionStorage.removeItem('username')
  window.location.href = '/'
}

onMounted(() => {
  // Connect to chat
  send({ type: 'CONNECT', username: props.username })
  
  // Simulate connection success after a delay
  setTimeout(() => {
    send({ type: 'CONNECTION_SUCCESS' })
    
    // Add some sample messages for demonstration
    const sampleMessages: MessageData[] = [
      {
        id: 'sample-1',
        username: 'SystemBot',
        content: 'Welcome to Enhanced Waddle Chat! Now with advanced XState actor model.',
        timestamp: Date.now() - 60000,
        category: 'General'
      },
      {
        id: 'sample-2',
        username: 'DevUser',
        content: 'Each message is now managed by its own XState actor with retry logic!',
        timestamp: Date.now() - 30000,
        category: 'Tech'
      }
    ]
    
    sampleMessages.forEach(message => {
      send({ type: 'RECEIVE_MESSAGE', message })
    })
  }, 1000)
})

onUnmounted(() => {
  send({ type: 'DISCONNECT' })
})
</script>
<template>
  <div class="h-screen flex flex-col bg-gradient-to-br from-black to-gray-900 relative overflow-hidden">
    <!-- Animated background -->
    <div class="absolute inset-0 bg-gradient-to-r from-gray-800/5 via-slate-800/5 to-gray-800/5"></div>
    <div class="absolute top-1/3 left-1/3 w-96 h-96 bg-white/3 rounded-full blur-3xl"></div>
    <div class="absolute bottom-1/3 right-1/3 w-96 h-96 bg-white/2 rounded-full blur-3xl"></div>

    <!-- Header -->
    <header class="relative z-10 p-4 bg-black/30 backdrop-blur-2xl border-b border-white/20 ring-1 ring-white/5">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-4">
          <h1 class="text-2xl font-bold text-white">
            Waddle Chat
          </h1>
          
          <!-- Connection Status -->
          <div class="flex items-center gap-2">
            <div :class="[
              'w-2 h-2 rounded-full',
              chatSnapshot?.context?.connectionStatus === 'connected' 
                ? 'bg-green-400 animate-pulse' 
                : chatSnapshot?.context?.connectionStatus === 'connecting'
                ? 'bg-yellow-400 animate-pulse'
                : 'bg-red-400'
            ]"></div>
            <span class="text-xs text-white/60 capitalize">
              {{ chatSnapshot?.context?.connectionStatus }}
            </span>
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
      <MessageList
        :messages="chatSnapshot?.context?.messages || []"
        :current-username="username"
        :active-filters="filterBar?.current?.context?.activeFilters || new Set()"
        :show-all="filterBar?.current?.context?.showAll || true"
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
      v-if="chatSnapshot?.context?.error"
      class="absolute top-20 left-1/2 transform -translate-x-1/2 z-20 bg-red-500/90 text-white px-4 py-2 rounded-lg backdrop-blur-sm"
    >
      {{ chatSnapshot.context.error }}
    </div>

    <!-- Subtle grid overlay -->
    <div class="absolute inset-0 opacity-20 bg-[url('/grid-pattern.svg')]"></div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { useMachine } from '@xstate/vue'
import { chatMachine } from '../machines/chatMachine'
import FilterBar from './FilterBar.vue'
import MessageList from './MessageList.vue'
import MessageInput from './MessageInput.vue'
import type { Message } from '../machines/chatMachine'

interface Props {
  username: string
}

const props = defineProps<Props>()

const { snapshot: chatSnapshot, send: chatSend } = useMachine(chatMachine)
const filterBar = ref<InstanceType<typeof FilterBar>>()
const messageInput = ref<InstanceType<typeof MessageInput>>()
const onlineCount = ref(1) // Placeholder for real user count

let ws: WebSocket | null = null
let reconnectTimeout: NodeJS.Timeout | null = null

const connectWebSocket = () => {
  try {
    chatSend({ type: 'CONNECT' })
    
    // Connect to Cloudflare Durable Object WebSocket endpoint
    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
    const wsUrl = `${protocol}//${location.host}/chat`
    ws = new WebSocket(wsUrl)
    
    ws.onopen = () => {
      chatSend({ type: 'CONNECTION_SUCCESS', socket: ws! })
      
      // Send join message
      ws?.send(JSON.stringify({
        type: 'join',
        username: props.username
      }))
    }
    
    ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data)
        
        if (data.type === 'message') {
          const message: Message = {
            id: data.id || Date.now().toString(),
            username: data.username,
            content: data.content,
            timestamp: data.timestamp || Date.now(),
            category: data.category || 'General'
          }
          chatSend({ type: 'MESSAGE_RECEIVED', message })
        } else if (data.type === 'messageHistory') {
          // Load existing messages when joining
          data.messages.forEach((message: Message) => {
            chatSend({ type: 'MESSAGE_RECEIVED', message })
          })
        } else if (data.type === 'userCount') {
          onlineCount.value = data.count
        }
      } catch (error) {
        console.error('Error parsing message:', error)
      }
    }
    
    ws.onerror = (error) => {
      console.error('WebSocket error:', error)
      chatSend({ type: 'CONNECTION_ERROR', error: 'Connection failed' })
    }
    
    ws.onclose = () => {
      chatSend({ type: 'DISCONNECT' })
      
      // Attempt to reconnect after 3 seconds
      reconnectTimeout = setTimeout(() => {
        console.log('Attempting to reconnect...')
        connectWebSocket()
      }, 3000)
    }
  } catch (error) {
    chatSend({ type: 'CONNECTION_ERROR', error: 'Failed to connect' })
  }
}

const handleSendMessage = ({ content, category }: { content: string; category: string }) => {
  if (ws && ws.readyState === WebSocket.OPEN) {
    chatSend({ type: 'SEND_MESSAGE', content, category })
    
    const messageData = {
      type: 'message',
      username: props.username,
      content,
      category,
      timestamp: Date.now(),
      id: Date.now().toString()
    }
    
    ws.send(JSON.stringify(messageData))
    
    // Simulate successful send for now
    setTimeout(() => {
      chatSend({ type: 'MESSAGE_SENT' })
      messageInput.value?.send({ type: 'SEND_SUCCESS' })
    }, 100)
  } else {
    chatSend({ type: 'MESSAGE_ERROR', error: 'Not connected' })
    messageInput.value?.send({ type: 'SEND_ERROR', error: 'Not connected' })
  }
}

const handleLogout = () => {
  if (ws) {
    ws.close()
  }
  sessionStorage.removeItem('username')
  window.location.href = '/'
}

onMounted(() => {
  // Connect to WebSocket when component mounts
  connectWebSocket()
})

onUnmounted(() => {
  if (ws) {
    ws.close()
  }
  if (reconnectTimeout) {
    clearTimeout(reconnectTimeout)
  }
})
</script>
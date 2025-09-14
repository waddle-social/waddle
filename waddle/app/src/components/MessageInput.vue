<template>
  <div class="p-4 bg-black/30 backdrop-blur-xl border-t border-white/20 ring-1 ring-white/5">
    <!-- Category Selection -->
    <div class="flex gap-2 mb-3 overflow-x-auto pb-2">
      <button
        v-for="category in CATEGORIES"
        :key="category"
        @click="send({ type: 'SET_CATEGORY', category })"
        :class="[
          'px-3 py-1 rounded-full text-xs font-medium whitespace-nowrap transition-all duration-200',
          snapshot?.context?.category === category
            ? 'bg-accent-primary text-white'
            : 'bg-white/10 text-white/70 hover:bg-white/20'
        ]"
      >
        {{ category }}
      </button>
    </div>

    <!-- Error Message -->
    <div v-if="snapshot?.context?.error" class="mb-3 text-red-400 text-sm bg-red-400/10 border border-red-400/20 rounded-lg p-2">
      {{ snapshot.context.error }}
    </div>

    <!-- Message Input -->
    <div class="flex gap-3">
      <div class="flex-1 relative">
        <input
          v-model="message"
          @input="send({ type: 'TYPE', content: $event.target.value })"
          @keydown.enter.prevent="handleSend"
          :disabled="snapshot?.matches?.('sending')"
          type="text"
          placeholder="Type your message..."
          class="w-full px-4 py-3 bg-white/5 border border-white/10 rounded-xl text-white placeholder-white/50 backdrop-blur-sm focus:outline-none focus:ring-2 focus:ring-accent-primary/50 focus:border-transparent transition-all duration-200 disabled:opacity-50"
        />
        <div class="absolute inset-0 rounded-xl bg-white/0 hover:bg-white/5 transition-all duration-300 pointer-events-none"></div>
      </div>
      
      <button
        @click="handleSend"
        :disabled="snapshot?.matches?.('sending') || !snapshot?.context?.content?.trim()"
        class="px-6 py-3 bg-accent-primary hover:bg-accent-primary-dark text-white font-semibold rounded-xl transition-all duration-200 transform hover:scale-105 active:scale-95 disabled:opacity-50 disabled:transform-none disabled:hover:scale-100 focus:outline-none focus:ring-2 focus:ring-accent-primary/50 shadow-lg"
      >
        <span v-if="snapshot?.matches?.('sending')" class="flex items-center">
          <svg class="animate-spin h-4 w-4" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
          </svg>
        </span>
        <span v-else>Send</span>
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'
import { useMachine } from '@xstate/vue'
import { messageMachine } from '../machines/messageMachine'
import { CATEGORIES } from '../machines/filterMachine'

const { snapshot, send } = useMachine(messageMachine)
const message = ref('')

const emit = defineEmits<{
  sendMessage: [{ content: string; category: string }]
}>()

// Sync local message with state machine
watch(() => snapshot.value?.context?.content, (newContent) => {
  if (newContent !== message.value) {
    message.value = newContent || ''
  }
})

const handleSend = () => {
  if (!snapshot.value?.context?.content?.trim()) return
  
  send({ type: 'SEND' })
  
  // Emit the message to parent component
  emit('sendMessage', {
    content: snapshot.value.context.content,
    category: snapshot.value.context.category
  })
}

// Expose machine state to parent
defineExpose({
  current: snapshot,
  send
})
</script>
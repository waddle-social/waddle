<template>
  <div class="chat-content">
    <!-- Chat Header -->
    <div class="chat-header">
      <div class="chat-info">
        <h3 class="chat-title"># general</h3>
        <p class="chat-description">Community discussions and announcements</p>
      </div>
      
      <div class="chat-actions">
        <!-- Connection Status -->
        <div class="connection-status">
          <div :class="[
            'status-indicator',
            connectionStatus === 'connected' ? 'connected' : 
            connectionStatus === 'connecting' ? 'connecting' : 'disconnected'
          ]"></div>
          <span class="status-text">{{ formatConnectionStatus(connectionStatus) }}</span>
        </div>
        
        <!-- Online Users -->
        <div class="online-users">
          <div class="user-avatars">
            <div 
              v-for="user in onlineUsers.slice(0, 5)" 
              :key="user.id"
              class="user-avatar"
              :title="user.displayName"
            >
              <img v-if="user.avatar" :src="user.avatar" :alt="user.displayName" />
              <div v-else class="avatar-placeholder">
                {{ user.displayName.charAt(0).toUpperCase() }}
              </div>
            </div>
            <div v-if="onlineUsers.length > 5" class="more-users">
              +{{ onlineUsers.length - 5 }}
            </div>
          </div>
          <span class="online-count">{{ onlineUsers.length }} online</span>
        </div>
        
        <!-- Chat Options -->
        <button class="chat-option-btn" title="Search messages">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <circle cx="11" cy="11" r="8"></circle>
            <path d="m21 21-4.35-4.35"></path>
          </svg>
        </button>
        
        <button class="chat-option-btn" title="Chat settings">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <circle cx="12" cy="12" r="3"></circle>
            <path d="M12 1v6M12 17v6M4.22 4.22l4.24 4.24M15.54 15.54l4.24 4.24M1 12h6M17 12h6M4.22 19.78l4.24-4.24M15.54 8.46l4.24-4.24"></path>
          </svg>
        </button>
      </div>
    </div>

    <!-- Messages Container -->
    <div class="messages-container" ref="messagesContainer">
      <div class="messages-list">
        <!-- Date Separator -->
        <div class="date-separator">
          <span>Today</span>
        </div>
        
        <!-- Messages -->
        <div 
          v-for="message in messages" 
          :key="message.id"
          :class="['message-item', { 'own-message': message.userId === currentUser.id }]"
        >
          <!-- User Avatar -->
          <div class="message-avatar">
            <img 
              v-if="message.avatar" 
              :src="message.avatar" 
              :alt="message.username"
            />
            <div v-else class="avatar-placeholder">
              {{ message.username.charAt(0).toUpperCase() }}
            </div>
          </div>
          
          <!-- Message Content -->
          <div class="message-content">
            <div class="message-header">
              <span class="username">{{ message.username }}</span>
              <span class="timestamp">{{ formatTime(message.timestamp) }}</span>
            </div>
            
            <div class="message-text">{{ message.content }}</div>
            
            <!-- Message Actions -->
            <div class="message-actions">
              <button @click="reactToMessage(message.id, '👍')" class="action-btn">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
                  <path d="M7 10v12l4-4 5-5v-3a1 1 0 0 0-1-1H9a1 1 0 0 0-1 1v1m-2 0V9a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v1m0 0v1a1 1 0 0 0 1 1h6"></path>
                </svg>
              </button>
              <button @click="replyToMessage(message)" class="action-btn">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
                  <path d="M3 20h7l5-5V9a1 1 0 0 0-1-1H4a1 1 0 0 0-1 1v10z"></path>
                </svg>
              </button>
              <button class="action-btn">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
                  <circle cx="12" cy="12" r="1"></circle>
                  <circle cx="19" cy="12" r="1"></circle>
                  <circle cx="5" cy="12" r="1"></circle>
                </svg>
              </button>
            </div>
          </div>
        </div>
        
        <!-- Typing Indicators -->
        <div v-if="typingUsers.length > 0" class="typing-indicator">
          <div class="typing-avatar">
            <div class="typing-dots">
              <span></span>
              <span></span>
              <span></span>
            </div>
          </div>
          <div class="typing-text">
            {{ formatTypingUsers(typingUsers) }}
          </div>
        </div>
      </div>
    </div>

    <!-- Message Input -->
    <div class="message-input-container">
      <div class="input-wrapper">
        <button class="attachment-btn" title="Add attachment">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"></path>
          </svg>
        </button>
        
        <div class="text-input-container">
          <textarea
            v-model="newMessage"
            @keydown="handleKeydown"
            @input="handleTyping"
            placeholder="Type your message..."
            class="message-input"
            rows="1"
            ref="messageInput"
          ></textarea>
        </div>
        
        <button 
          @click="sendMessage"
          :disabled="!newMessage.trim() || sending"
          class="send-btn"
          title="Send message"
        >
          <svg v-if="!sending" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <line x1="22" y1="2" x2="11" y2="13"></line>
            <polygon points="22,2 15,22 11,13 2,9 22,2"></polygon>
          </svg>
          <div v-else class="loading-spinner"></div>
        </button>
      </div>
      
      <!-- Message Formatting Options -->
      <div v-if="showFormatting" class="formatting-options">
        <button @click="formatText('bold')" class="format-btn">
          <strong>B</strong>
        </button>
        <button @click="formatText('italic')" class="format-btn">
          <em>I</em>
        </button>
        <button @click="formatText('code')" class="format-btn">
          &lt;/&gt;
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, nextTick, watch } from 'vue'
import type { User } from '../types/content'

interface Message {
  id: string
  userId: string
  username: string
  avatar?: string
  content: string
  timestamp: number
  type?: 'message' | 'system' | 'join' | 'leave'
  replyTo?: string
}

interface Props {
  currentUser: User
  messages?: Message[]
  onlineUsers?: User[]
  connectionStatus?: 'connected' | 'connecting' | 'disconnected'
}

const props = withDefaults(defineProps<Props>(), {
  messages: () => [],
  onlineUsers: () => [],
  connectionStatus: 'disconnected'
})

const emit = defineEmits<{
  sendMessage: [message: string]
  typing: [isTyping: boolean]
  react: [messageId: string, reaction: string]
  reply: [message: Message]
}>()

// Component state
const newMessage = ref('')
const sending = ref(false)
const showFormatting = ref(false)
const typingUsers = ref<User[]>([])
const messagesContainer = ref<HTMLElement>()
const messageInput = ref<HTMLTextAreaElement>()

// Mock messages for demo
const messages = ref<Message[]>([
  {
    id: '1',
    userId: 'user_1',
    username: 'alice',
    content: 'Hey everyone! Welcome to the new chat design 🎉',
    timestamp: Date.now() - 300000
  },
  {
    id: '2',
    userId: 'user_2',
    username: 'bob',
    avatar: '/avatars/bob.jpg',
    content: 'This looks amazing! Much better integrated with the sidebar.',
    timestamp: Date.now() - 240000
  },
  {
    id: '3',
    userId: 'user_3',
    username: 'charlie',
    content: 'I love how clean this layout is. Great work on the redesign!',
    timestamp: Date.now() - 180000
  },
  {
    id: '4',
    userId: props.currentUser.id,
    username: props.currentUser.username,
    content: 'Thanks! The sidebar integration was exactly what we needed.',
    timestamp: Date.now() - 120000
  }
])

const onlineUsers = ref<User[]>([
  { id: 'user_1', username: 'alice', displayName: 'Alice', email: 'alice@example.com', joinedAt: Date.now(), preferences: {} as any },
  { id: 'user_2', username: 'bob', displayName: 'Bob', email: 'bob@example.com', joinedAt: Date.now(), preferences: {} as any },
  { id: 'user_3', username: 'charlie', displayName: 'Charlie', email: 'charlie@example.com', joinedAt: Date.now(), preferences: {} as any },
  props.currentUser
])

// Methods
const formatTime = (timestamp: number) => {
  const date = new Date(timestamp)
  const now = new Date()
  const isToday = date.toDateString() === now.toDateString()
  
  if (isToday) {
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  } else {
    return date.toLocaleDateString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
  }
}

const formatConnectionStatus = (status: string) => {
  return status.charAt(0).toUpperCase() + status.slice(1)
}

const formatTypingUsers = (users: User[]) => {
  if (users.length === 1) {
    return `${users[0].displayName} is typing...`
  } else if (users.length === 2) {
    return `${users[0].displayName} and ${users[1].displayName} are typing...`
  } else {
    return 'Several people are typing...'
  }
}

const sendMessage = async () => {
  if (!newMessage.value.trim() || sending.value) return
  
  sending.value = true
  
  try {
    // Add message locally first for immediate feedback
    const message: Message = {
      id: `msg_${Date.now()}`,
      userId: props.currentUser.id,
      username: props.currentUser.username,
      avatar: props.currentUser.avatar,
      content: newMessage.value.trim(),
      timestamp: Date.now()
    }
    
    messages.value.push(message)
    emit('sendMessage', newMessage.value)
    newMessage.value = ''
    
    // Auto-resize textarea
    if (messageInput.value) {
      messageInput.value.style.height = 'auto'
    }
    
    await nextTick()
    scrollToBottom()
  } finally {
    sending.value = false
  }
}

const handleKeydown = (event: KeyboardEvent) => {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault()
    sendMessage()
  }
  
  // Auto-resize textarea
  nextTick(() => {
    if (messageInput.value) {
      messageInput.value.style.height = 'auto'
      messageInput.value.style.height = messageInput.value.scrollHeight + 'px'
    }
  })
}

const handleTyping = () => {
  emit('typing', true)
  // In real app, you'd debounce this and emit false after a delay
}

const reactToMessage = (messageId: string, reaction: string) => {
  emit('react', messageId, reaction)
}

const replyToMessage = (message: Message) => {
  emit('reply', message)
}

const formatText = (format: 'bold' | 'italic' | 'code') => {
  // Text formatting logic would go here
  console.log('Format text:', format)
}

const scrollToBottom = () => {
  if (messagesContainer.value) {
    messagesContainer.value.scrollTop = messagesContainer.value.scrollHeight
  }
}

// Watch for new messages and scroll
watch(messages, () => {
  nextTick(() => scrollToBottom())
}, { deep: true })

onMounted(() => {
  scrollToBottom()
})
</script>

<style scoped>
.chat-content {
  display: flex;
  flex-direction: column;
  height: calc(100vh - 120px);
  background: rgba(255, 255, 255, 0.02);
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  overflow: hidden;
}

.chat-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 1rem 1.5rem;
  background: rgba(0, 0, 0, 0.3);
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
  backdrop-filter: blur(10px);
}

.chat-info {
  flex: 1;
  min-width: 0;
}

.chat-title {
  font-size: 1.25rem;
  font-weight: 600;
  color: white;
  margin: 0 0 0.25rem 0;
}

.chat-description {
  font-size: 0.875rem;
  color: rgba(255, 255, 255, 0.6);
  margin: 0;
}

.chat-actions {
  display: flex;
  align-items: center;
  gap: 1rem;
}

.connection-status {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.status-indicator {
  width: 8px;
  height: 8px;
  border-radius: 50%;
}

.status-indicator.connected {
  background: #22c55e;
  animation: pulse 2s infinite;
}

.status-indicator.connecting {
  background: #f59e0b;
  animation: pulse 1s infinite;
}

.status-indicator.disconnected {
  background: #ef4444;
}

.status-text {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.6);
  text-transform: capitalize;
}

.online-users {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.user-avatars {
  display: flex;
  align-items: center;
}

.user-avatar {
  width: 24px;
  height: 24px;
  border-radius: 50%;
  border: 2px solid rgba(0, 0, 0, 0.3);
  margin-left: -4px;
  overflow: hidden;
}

.user-avatar:first-child {
  margin-left: 0;
}

.user-avatar img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.avatar-placeholder {
  width: 100%;
  height: 100%;
  background: var(--accent-primary);
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  font-size: 0.6rem;
  font-weight: 600;
}

.more-users {
  width: 24px;
  height: 24px;
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.2);
  border: 2px solid rgba(0, 0, 0, 0.3);
  margin-left: -4px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 0.6rem;
  color: rgba(255, 255, 255, 0.8);
  font-weight: 600;
}

.online-count {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.6);
}

.chat-option-btn {
  width: 32px;
  height: 32px;
  padding: 0;
  background: none;
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 6px;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s ease;
}

.chat-option-btn:hover {
  background: rgba(255, 255, 255, 0.05);
  color: white;
  border-color: rgba(255, 255, 255, 0.3);
}

.chat-option-btn svg {
  width: 16px;
  height: 16px;
}

.messages-container {
  flex: 1;
  overflow-y: auto;
  padding: 0;
}

.messages-list {
  padding: 1rem 0;
}

.date-separator {
  text-align: center;
  margin: 1rem 0;
  position: relative;
}

.date-separator::before {
  content: '';
  position: absolute;
  top: 50%;
  left: 0;
  right: 0;
  height: 1px;
  background: rgba(255, 255, 255, 0.1);
}

.date-separator span {
  background: rgba(0, 0, 0, 0.8);
  color: rgba(255, 255, 255, 0.6);
  padding: 0.25rem 1rem;
  border-radius: 12px;
  font-size: 0.75rem;
  font-weight: 500;
  position: relative;
  z-index: 1;
}

.message-item {
  display: flex;
  gap: 0.75rem;
  padding: 0.75rem 1.5rem;
  transition: all 0.2s ease;
}

.message-item:hover {
  background: rgba(255, 255, 255, 0.02);
}

.message-item.own-message {
  background: rgba(var(--accent-primary-rgb), 0.05);
}

.message-avatar {
  width: 40px;
  height: 40px;
  border-radius: 50%;
  overflow: hidden;
  flex-shrink: 0;
}

.message-avatar img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.message-avatar .avatar-placeholder {
  width: 100%;
  height: 100%;
  background: var(--accent-primary);
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  font-weight: 600;
}

.message-content {
  flex: 1;
  min-width: 0;
}

.message-header {
  display: flex;
  align-items: baseline;
  gap: 0.5rem;
  margin-bottom: 0.25rem;
}

.username {
  font-weight: 600;
  color: white;
  font-size: 0.875rem;
}

.timestamp {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.4);
}

.message-text {
  color: rgba(255, 255, 255, 0.9);
  line-height: 1.5;
  word-wrap: break-word;
}

.message-actions {
  display: flex;
  gap: 0.25rem;
  margin-top: 0.5rem;
  opacity: 0;
  transition: opacity 0.2s ease;
}

.message-item:hover .message-actions {
  opacity: 1;
}

.action-btn {
  width: 28px;
  height: 28px;
  padding: 0;
  background: none;
  border: 1px solid transparent;
  border-radius: 4px;
  color: rgba(255, 255, 255, 0.4);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s ease;
}

.action-btn:hover {
  background: rgba(255, 255, 255, 0.05);
  color: rgba(255, 255, 255, 0.8);
  border-color: rgba(255, 255, 255, 0.2);
}

.action-btn svg {
  width: 14px;
  height: 14px;
}

.typing-indicator {
  display: flex;
  gap: 0.75rem;
  padding: 0.75rem 1.5rem;
  opacity: 0.7;
}

.typing-avatar {
  width: 40px;
  height: 40px;
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.1);
  display: flex;
  align-items: center;
  justify-content: center;
}

.typing-dots {
  display: flex;
  gap: 2px;
}

.typing-dots span {
  width: 4px;
  height: 4px;
  background: rgba(255, 255, 255, 0.6);
  border-radius: 50%;
  animation: typing 1.4s infinite;
}

.typing-dots span:nth-child(2) {
  animation-delay: 0.2s;
}

.typing-dots span:nth-child(3) {
  animation-delay: 0.4s;
}

.typing-text {
  color: rgba(255, 255, 255, 0.6);
  font-size: 0.875rem;
  display: flex;
  align-items: center;
}

.message-input-container {
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(0, 0, 0, 0.2);
  padding: 1rem 1.5rem;
}

.input-wrapper {
  display: flex;
  align-items: flex-end;
  gap: 0.75rem;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  padding: 0.75rem;
}

.input-wrapper:focus-within {
  border-color: var(--accent-primary);
  background: rgba(255, 255, 255, 0.08);
}

.attachment-btn {
  width: 32px;
  height: 32px;
  padding: 0;
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  border-radius: 6px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s ease;
  flex-shrink: 0;
}

.attachment-btn:hover {
  background: rgba(255, 255, 255, 0.1);
  color: white;
}

.attachment-btn svg {
  width: 18px;
  height: 18px;
}

.text-input-container {
  flex: 1;
  min-width: 0;
}

.message-input {
  width: 100%;
  background: none;
  border: none;
  color: white;
  font-size: 0.875rem;
  line-height: 1.5;
  resize: none;
  outline: none;
  min-height: 20px;
  max-height: 120px;
  overflow-y: auto;
}

.message-input::placeholder {
  color: rgba(255, 255, 255, 0.4);
}

.send-btn {
  width: 32px;
  height: 32px;
  padding: 0;
  background: var(--accent-primary);
  border: none;
  border-radius: 6px;
  color: white;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s ease;
  flex-shrink: 0;
}

.send-btn:hover:not(:disabled) {
  background: var(--accent-hover);
  transform: translateY(-1px);
}

.send-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
  transform: none;
}

.send-btn svg {
  width: 16px;
  height: 16px;
}

.loading-spinner {
  width: 16px;
  height: 16px;
  border: 2px solid transparent;
  border-top: 2px solid white;
  border-radius: 50%;
  animation: spin 1s linear infinite;
}

.formatting-options {
  display: flex;
  gap: 0.5rem;
  margin-top: 0.5rem;
}

.format-btn {
  width: 28px;
  height: 28px;
  padding: 0;
  background: none;
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 4px;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 0.75rem;
  transition: all 0.2s ease;
}

.format-btn:hover {
  background: rgba(255, 255, 255, 0.05);
  color: white;
  border-color: rgba(255, 255, 255, 0.3);
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.5; }
}

@keyframes typing {
  0%, 60%, 100% { transform: translateY(0); }
  30% { transform: translateY(-10px); }
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

/* Scrollbar styling */
.messages-container::-webkit-scrollbar {
  width: 8px;
}

.messages-container::-webkit-scrollbar-track {
  background: rgba(255, 255, 255, 0.05);
  border-radius: 4px;
}

.messages-container::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.2);
  border-radius: 4px;
}

.messages-container::-webkit-scrollbar-thumb:hover {
  background: rgba(255, 255, 255, 0.3);
}

/* Responsive design */
@media (max-width: 768px) {
  .chat-header {
    padding: 0.75rem 1rem;
  }
  
  .chat-actions {
    gap: 0.5rem;
  }
  
  .online-users .user-avatars {
    display: none;
  }
  
  .message-item {
    padding: 0.75rem 1rem;
  }
  
  .message-input-container {
    padding: 0.75rem 1rem;
  }
}
</style>
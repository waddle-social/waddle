<template>
  <div class="direct-message-content">
    <!-- Message Header -->
    <header class="message-header">
      <div class="sender-info">
        <img 
          v-if="message.avatar" 
          :src="message.avatar" 
          :alt="message.username"
          class="sender-avatar"
        />
        <div v-else class="sender-avatar-placeholder">
          {{ message.username.charAt(0).toUpperCase() }}
        </div>
        <div class="sender-details">
          <span class="sender-name">{{ message.username }}</span>
          <div class="message-meta">
            <time class="timestamp" :datetime="new Date(message.timestamp).toISOString()">
              {{ formatTime(message.timestamp) }}
            </time>
            <span v-if="message.isGroupMessage" class="group-indicator">
              to {{ formatParticipants(message.participants) }}
            </span>
          </div>
        </div>
      </div>
      
      <div class="message-status">
        <div v-if="message.isRead" class="status-read" title="Read">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <polyline points="20 6 9 17 4 12"></polyline>
            <polyline points="16 10 21 5 8 18 3 13"></polyline>
          </svg>
        </div>
        <div v-else-if="message.deliveredAt" class="status-delivered" title="Delivered">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <polyline points="20 6 9 17 4 12"></polyline>
          </svg>
        </div>
        <div v-else class="status-sending" title="Sending">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <circle cx="12" cy="12" r="10"></circle>
            <path d="M12 6v6l4 2"></path>
          </svg>
        </div>
      </div>
    </header>

    <!-- Message Content -->
    <main class="message-body">
      <div class="message-text" v-html="formatMessageContent(message.content)"></div>
      
      <!-- Attachments -->
      <div v-if="message.attachments && message.attachments.length > 0" class="attachments">
        <div 
          v-for="attachment in message.attachments" 
          :key="attachment.id"
          class="attachment-item"
        >
          <AttachmentPreview :attachment="attachment" />
        </div>
      </div>
      
      <!-- Group Message Info -->
      <div v-if="message.isGroupMessage" class="group-info">
        <div class="group-participants">
          <svg class="participants-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
            <circle cx="9" cy="7" r="4"></circle>
            <path d="M23 21v-2a4 4 0 0 0-3-3.87"></path>
            <path d="M16 3.13a4 4 0 0 1 0 7.75"></path>
          </svg>
          <span class="participants-count">
            {{ message.participants.length }} participants
          </span>
        </div>
        
        <div class="conversation-link">
          <button @click="openConversation" class="open-conversation-btn">
            View full conversation
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"></path>
              <polyline points="15,3 21,3 21,9"></polyline>
              <line x1="10" y1="14" x2="21" y2="3"></line>
            </svg>
          </button>
        </div>
      </div>
    </main>

    <!-- Message Actions -->
    <footer v-if="interactive" class="message-actions">
      <button @click="handleReply" class="action-btn reply-btn">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path d="M3 10h10a8 8 0 0 1 8 8v2"></path>
          <polyline points="3 10 7 6 7 14 3 10"></polyline>
        </svg>
        Reply
      </button>
      
      <button @click="handleForward" class="action-btn forward-btn">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path d="M21 10h-10a8 8 0 0 0-8 8v2"></path>
          <polyline points="21 10 17 6 17 14 21 10"></polyline>
        </svg>
        Forward
      </button>
      
      <button @click="handleMarkImportant" class="action-btn important-btn">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"></polygon>
        </svg>
        Important
      </button>
      
      <div class="message-actions-more">
        <button @click="showMoreActions = !showMoreActions" class="action-btn more-btn">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <circle cx="12" cy="12" r="1"></circle>
            <circle cx="19" cy="12" r="1"></circle>
            <circle cx="5" cy="12" r="1"></circle>
          </svg>
        </button>
        
        <!-- More Actions Menu -->
        <div v-if="showMoreActions" class="more-actions-menu">
          <button @click="handleCopy" class="menu-item">
            <span class="menu-icon">📋</span>
            Copy message
          </button>
          <button @click="handleDelete" class="menu-item danger">
            <span class="menu-icon">🗑️</span>
            Delete
          </button>
          <button @click="handleReport" class="menu-item danger">
            <span class="menu-icon">🚨</span>
            Report
          </button>
        </div>
      </div>
    </footer>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import type { DirectMessage, LayoutMode } from '../../types/content'
import AttachmentPreview from '../AttachmentPreview.vue'

interface Props {
  message: DirectMessage
  layout: LayoutMode
  interactive: boolean
}

const props = defineProps<Props>()

const emit = defineEmits<{
  react: [reaction: string]
  reply: [replyText: string]
}>()

const showMoreActions = ref(false)

// Format message content with basic markdown-like support
const formatMessageContent = (content: string) => {
  return content
    .replace(/\*\*(.*?)\*\*/g, '<strong>$1</strong>')
    .replace(/\*(.*?)\*/g, '<em>$1</em>')
    .replace(/`(.*?)`/g, '<code>$1</code>')
    .replace(/(https?:\/\/[^\s]+)/g, '<a href="$1" target="_blank" rel="noopener noreferrer">$1</a>')
    .replace(/@(\w+)/g, '<span class="mention">@$1</span>')
    .replace(/\n/g, '<br>')
}

const formatTime = (timestamp: number) => {
  const now = Date.now()
  const diff = now - timestamp
  const minutes = Math.floor(diff / 60000)
  const hours = Math.floor(minutes / 60)
  const days = Math.floor(hours / 24)
  
  if (days > 0) return `${days}d ago`
  if (hours > 0) return `${hours}h ago`
  if (minutes > 0) return `${minutes}m ago`
  return 'Just now'
}

const formatParticipants = (participants: string[]) => {
  const others = participants.filter(p => p !== props.message.username)
  if (others.length === 1) return others[0]
  if (others.length === 2) return `${others[0]} and ${others[1]}`
  return `${others[0]} and ${others.length - 1} others`
}

const handleReply = () => {
  const replyText = prompt('Reply to this message:')
  if (replyText) {
    emit('reply', replyText)
  }
}

const handleForward = () => {
  // In a real app, this would open a forward interface
  alert('Forward functionality would be implemented here')
}

const handleMarkImportant = () => {
  // In a real app, this would mark the message as important
  alert('Message marked as important')
}

const handleCopy = () => {
  navigator.clipboard?.writeText(props.message.content)
  alert('Message copied to clipboard')
  showMoreActions.value = false
}

const handleDelete = () => {
  if (confirm('Are you sure you want to delete this message?')) {
    // In a real app, this would delete the message
    alert('Message deleted')
  }
  showMoreActions.value = false
}

const handleReport = () => {
  const reason = prompt('Please specify the reason for reporting this message:')
  if (reason) {
    // In a real app, this would report the message
    alert('Message reported')
  }
  showMoreActions.value = false
}

const openConversation = () => {
  // In a real app, this would navigate to the full conversation
  alert(`Opening conversation: ${props.message.conversationId}`)
}

// Click outside handler
const handleClickOutside = (event: Event) => {
  const target = event.target as HTMLElement
  if (!target.closest('.message-actions-more')) {
    showMoreActions.value = false
  }
}

onMounted(() => {
  document.addEventListener('click', handleClickOutside)
})

onUnmounted(() => {
  document.removeEventListener('click', handleClickOutside)
})
</script>

<style scoped>
.direct-message-content {
  padding: 1rem;
  color: white;
  background: rgba(255, 255, 255, 0.02);
  border-left: 3px solid var(--accent-primary);
}

.message-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 0.75rem;
}

.sender-info {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  flex: 1;
}

.sender-avatar {
  width: 32px;
  height: 32px;
  border-radius: 50%;
  object-fit: cover;
}

.sender-avatar-placeholder {
  width: 32px;
  height: 32px;
  background: var(--accent-primary);
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  font-weight: 600;
  font-size: 0.9rem;
}

.sender-details {
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
  flex: 1;
}

.sender-name {
  font-weight: 600;
  font-size: 0.9rem;
  color: white;
}

.message-meta {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
}

.group-indicator {
  color: rgba(255, 255, 255, 0.6);
}

.message-status {
  flex-shrink: 0;
}

.status-read,
.status-delivered,
.status-sending {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
}

.status-read {
  color: #22c55e;
}

.status-delivered {
  color: rgba(255, 255, 255, 0.6);
}

.status-sending {
  color: rgba(255, 255, 255, 0.4);
  animation: spin 1s linear infinite;
}

.status-read svg,
.status-delivered svg,
.status-sending svg {
  width: 14px;
  height: 14px;
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

.message-body {
  margin-bottom: 0.75rem;
}

.message-text {
  line-height: 1.5;
  word-wrap: break-word;
  margin-bottom: 0.75rem;
}

.message-text :deep(strong) {
  font-weight: 700;
  color: white;
}

.message-text :deep(em) {
  font-style: italic;
  color: rgba(255, 255, 255, 0.9);
}

.message-text :deep(code) {
  background: rgba(255, 255, 255, 0.1);
  padding: 0.125rem 0.25rem;
  border-radius: 4px;
  font-family: monospace;
  font-size: 0.85em;
}

.message-text :deep(a) {
  color: var(--accent-primary);
  text-decoration: none;
}

.message-text :deep(a:hover) {
  text-decoration: underline;
}

.message-text :deep(.mention) {
  background: rgba(var(--accent-primary-rgb), 0.2);
  color: var(--accent-primary);
  padding: 0.125rem 0.25rem;
  border-radius: 4px;
  font-weight: 500;
}

.attachments {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  margin-bottom: 0.75rem;
}

.attachment-item {
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  padding: 0.75rem;
}

.group-info {
  background: rgba(255, 255, 255, 0.03);
  border-radius: 8px;
  padding: 0.75rem;
  margin-bottom: 0.75rem;
}

.group-participants {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-bottom: 0.5rem;
}

.participants-icon {
  width: 16px;
  height: 16px;
  color: rgba(255, 255, 255, 0.6);
}

.participants-count {
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.7);
}

.conversation-link {
  display: flex;
  justify-content: flex-end;
}

.open-conversation-btn {
  display: flex;
  align-items: center;
  gap: 0.375rem;
  background: none;
  border: none;
  color: var(--accent-primary);
  cursor: pointer;
  font-size: 0.8rem;
  font-weight: 500;
  padding: 0.25rem 0.5rem;
  border-radius: 6px;
  transition: all 0.2s ease;
}

.open-conversation-btn:hover {
  background: rgba(var(--accent-primary-rgb), 0.1);
}

.open-conversation-btn svg {
  width: 14px;
  height: 14px;
}

.message-actions {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding-top: 0.75rem;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  flex-wrap: wrap;
}

.action-btn {
  display: flex;
  align-items: center;
  gap: 0.375rem;
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  padding: 0.375rem 0.75rem;
  border-radius: 6px;
  font-size: 0.8rem;
  font-weight: 500;
  transition: all 0.2s ease;
}

.action-btn:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.action-btn svg {
  width: 14px;
  height: 14px;
}

.message-actions-more {
  position: relative;
  margin-left: auto;
}

.more-actions-menu {
  position: absolute;
  bottom: 100%;
  right: 0;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  margin-bottom: 0.5rem;
  overflow: hidden;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
  z-index: 100;
  min-width: 140px;
}

.menu-item {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  width: 100%;
  padding: 0.75rem 1rem;
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.8);
  cursor: pointer;
  font-size: 0.9rem;
  text-align: left;
  transition: background-color 0.15s ease;
}

.menu-item:hover {
  background: rgba(255, 255, 255, 0.1);
  color: white;
}

.menu-item.danger {
  color: #ef4444;
}

.menu-item.danger:hover {
  background: rgba(239, 68, 68, 0.1);
}

.menu-icon {
  font-size: 0.9rem;
  opacity: 0.8;
}

@media (max-width: 640px) {
  .message-actions {
    flex-direction: column;
    align-items: stretch;
    gap: 0.5rem;
  }
  
  .action-btn {
    justify-content: center;
  }
  
  .message-actions-more {
    margin-left: 0;
    align-self: center;
  }
}
</style>
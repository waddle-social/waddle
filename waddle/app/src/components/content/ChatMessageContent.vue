<template>
  <div class="chat-message-content">
    <!-- User Header -->
    <header class="message-header">
      <div class="user-info">
        <img 
          v-if="message.avatar" 
          :src="message.avatar" 
          :alt="message.username"
          class="user-avatar"
        />
        <div v-else class="user-avatar-placeholder">
          {{ message.username.charAt(0).toUpperCase() }}
        </div>
        <div class="user-details">
          <span class="username">{{ message.username }}</span>
          <time class="timestamp" :datetime="new Date(message.timestamp).toISOString()">
            {{ formatTime(message.timestamp) }}
          </time>
        </div>
      </div>
      
      <div v-if="message.category" class="message-category">
        <span class="category-badge">{{ message.category }}</span>
      </div>
    </header>

    <!-- Message Content -->
    <main class="message-body">
      <div class="message-text" v-html="formatMessageContent(message.content)"></div>
      
      <!-- Edited Indicator -->
      <div v-if="message.isEdited" class="edited-indicator">
        <span class="edited-text">edited</span>
        <time v-if="message.editedAt" class="edited-time">
          {{ formatTime(message.editedAt) }}
        </time>
      </div>
      
      <!-- Mentions -->
      <div v-if="message.mentions && message.mentions.length > 0" class="mentions">
        <span class="mentions-label">Mentions:</span>
        <span 
          v-for="mention in message.mentions" 
          :key="mention" 
          class="mention-tag"
        >
          @{{ mention }}
        </span>
      </div>
      
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
    </main>

    <!-- Reply Reference -->
    <aside v-if="message.replyTo" class="reply-reference">
      <div class="reply-indicator">↳ Reply to:</div>
      <div class="reply-preview">
        <!-- This would fetch and display the referenced message -->
        <span class="reply-id">{{ message.replyTo }}</span>
      </div>
    </aside>

    <!-- Interactive Actions -->
    <footer v-if="interactive" class="message-actions">
      <button @click="handleReply" class="action-btn reply-btn">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path d="M9 17l6-6-6-6"></path>
        </svg>
        Reply
      </button>
      
      <button @click="handleQuote" class="action-btn quote-btn">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path d="M3 21c3 0 7-1 7-8V5c0-1.25-.756-2.017-2-2H4c-1.25 0-2 .75-2 1.972V11c0 1.25.75 2 2 2 1 0 1 0 1 1v1c0 1-1 2-2 2s-1 .008-1 1.031V20c0 1 0 1 1 1z"></path>
          <path d="M15 21c3 0 7-1 7-8V5c0-1.25-.757-2.017-2-2h-4c-1.25 0-2 .75-2 1.972V11c0 1.25.75 2 2 2h.75c0 2.25.25 4-2.75 4v3c0 1 0 1 1 1z"></path>
        </svg>
        Quote
      </button>
    </footer>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { ChatMessage, LayoutMode } from '../../types/content'
import AttachmentPreview from '../AttachmentPreview.vue'

interface Props {
  message: ChatMessage
  layout: LayoutMode
  interactive: boolean
}

const props = defineProps<Props>()

const emit = defineEmits<{
  react: [reaction: string]
  reply: [replyText: string]
  bookmark: []
}>()

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

const handleReply = () => {
  // In a real app, this would open a reply interface
  const replyText = prompt('Reply to this message:')
  if (replyText) {
    emit('reply', replyText)
  }
}

const handleQuote = () => {
  // Copy message content for quoting
  navigator.clipboard?.writeText(`> ${props.message.content}\n\n`)
}
</script>

<style scoped>
.chat-message-content {
  padding: 1rem;
  color: white;
}

.message-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 0.75rem;
}

.user-info {
  display: flex;
  align-items: center;
  gap: 0.75rem;
}

.user-avatar {
  width: 32px;
  height: 32px;
  border-radius: 50%;
  object-fit: cover;
}

.user-avatar-placeholder {
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

.user-details {
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
}

.username {
  font-weight: 600;
  font-size: 0.9rem;
  color: white;
}

.timestamp {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
}

.message-category {
  flex-shrink: 0;
}

.category-badge {
  background: rgba(var(--accent-primary-rgb), 0.2);
  color: var(--accent-primary);
  padding: 0.25rem 0.5rem;
  border-radius: 12px;
  font-size: 0.7rem;
  font-weight: 500;
  text-transform: uppercase;
  letter-spacing: 0.025em;
}

.message-body {
  margin-bottom: 0.75rem;
}

.message-text {
  line-height: 1.5;
  word-wrap: break-word;
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

.edited-indicator {
  margin-top: 0.5rem;
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
  font-style: italic;
}

.mentions {
  margin-top: 0.5rem;
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex-wrap: wrap;
}

.mentions-label {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.6);
}

.mention-tag {
  background: rgba(var(--accent-primary-rgb), 0.2);
  color: var(--accent-primary);
  padding: 0.125rem 0.375rem;
  border-radius: 8px;
  font-size: 0.75rem;
  font-weight: 500;
}

.attachments {
  margin-top: 0.75rem;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.attachment-item {
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  padding: 0.75rem;
}

.reply-reference {
  background: rgba(255, 255, 255, 0.05);
  border-left: 3px solid var(--accent-primary);
  border-radius: 0 8px 8px 0;
  padding: 0.5rem 0.75rem;
  margin-bottom: 0.75rem;
}

.reply-indicator {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.6);
  margin-bottom: 0.25rem;
}

.reply-preview {
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.8);
}

.reply-id {
  font-family: monospace;
  color: rgba(255, 255, 255, 0.5);
}

.message-actions {
  display: flex;
  gap: 0.5rem;
  padding-top: 0.5rem;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}

.action-btn {
  display: flex;
  align-items: center;
  gap: 0.25rem;
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
</style>
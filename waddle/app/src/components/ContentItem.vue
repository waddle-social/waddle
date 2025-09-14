<template>
  <article 
    :class="[
      'content-item',
      `content-${item.content.type}`,
      `layout-${layout}`,
      { 'interactive': interactive },
      { 'bookmarked': isBookmarked },
      { 'pinned': isPinned },
      { 'trending': item.isTrending },
      { 'promoted': item.isPromoted },
    ]"
    :data-content-id="item.content.id"
    :data-content-type="item.content.type"
  >
    <!-- Content Type Indicator -->
    <div class="content-indicator">
      <span class="content-type-icon">{{ getTypeIcon(item.content.type) }}</span>
      <span v-if="item.isTrending" class="trending-badge">🔥</span>
      <span v-if="item.isPromoted" class="promoted-badge">⭐</span>
    </div>

    <!-- Chat Message Content -->
    <ChatMessageContent
      v-if="item.content.type === 'chat'"
      :message="item.content as ChatMessage"
      :layout="layout"
      :interactive="interactive"
      @react="handleReaction"
      @reply="handleReply"
      @bookmark="handleBookmark"
    />

    <!-- Event Content -->
    <EventContent
      v-else-if="item.content.type === 'event'"
      :event="item.content as Event"
      :layout="layout"
      :interactive="interactive"
      @rsvp="handleRSVP"
      @bookmark="handleBookmark"
      @share="handleShare"
    />

    <!-- Link Content -->
    <LinkContent
      v-else-if="item.content.type === 'link'"
      :link="item.content as Link"
      :layout="layout"
      :interactive="interactive"
      @vote="handleVote"
      @comment="handleComment"
      @bookmark="handleBookmark"
      @share="handleShare"
    />

    <!-- Person Content -->
    <PersonContent
      v-else-if="item.content.type === 'person'"
      :person="item.content as Person"
      :layout="layout"
      :interactive="interactive"
      @follow="handleFollow"
      @message="handleDirectMessage"
      @connect="handleConnect"
    />

    <!-- Hangout Content -->
    <HangoutContent
      v-else-if="item.content.type === 'hangout'"
      :hangout="item.content as Hangout"
      :layout="layout"
      :interactive="interactive"
      @join="handleJoinHangout"
      @bookmark="handleBookmark"
      @share="handleShare"
    />

    <!-- Direct Message Content -->
    <DirectMessageContent
      v-else-if="item.content.type === 'message'"
      :message="item.content as DirectMessage"
      :layout="layout"
      :interactive="interactive"
      @react="handleReaction"
      @reply="handleReply"
    />

    <!-- Fallback for Unknown Content Types -->
    <div v-else class="unknown-content">
      <div class="unknown-header">
        <span class="unknown-type">{{ item.content.type }}</span>
        <span class="unknown-id">{{ item.content.id }}</span>
      </div>
      <pre class="unknown-data">{{ JSON.stringify(item.content, null, 2) }}</pre>
    </div>

    <!-- Global Actions Bar -->
    <footer v-if="interactive" class="content-actions">
      <div class="action-group primary-actions">
        <!-- Reactions -->
        <div class="reactions-container">
          <button
            v-for="(count, reaction) in item.content.reactions"
            :key="reaction"
            @click="handleReaction(reaction)"
            :class="[
              'reaction-button',
              { 'user-reacted': item.content.userReaction === reaction }
            ]"
          >
            <span class="reaction-emoji">{{ reaction }}</span>
            <span class="reaction-count">{{ count }}</span>
          </button>
          
          <button @click="showReactionPicker = !showReactionPicker" class="add-reaction">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <circle cx="12" cy="12" r="10"></circle>
              <path d="M8 14s1.5 2 4 2 4-2 4-2"></path>
              <line x1="9" y1="9" x2="9.01" y2="9"></line>
              <line x1="15" y1="9" x2="15.01" y2="9"></line>
            </svg>
          </button>

          <!-- Reaction Picker -->
          <div v-if="showReactionPicker" class="reaction-picker">
            <button
              v-for="emoji in commonReactions"
              :key="emoji"
              @click="handleReaction(emoji)"
              class="picker-emoji"
            >
              {{ emoji }}
            </button>
          </div>
        </div>
      </div>

      <div class="action-group secondary-actions">
        <!-- Bookmark Toggle -->
        <button
          @click="handleBookmark"
          :class="['action-button', { 'active': isBookmarked }]"
          :title="isBookmarked ? 'Remove bookmark' : 'Bookmark'"
        >
          <svg viewBox="0 0 24 24" :fill="isBookmarked ? 'currentColor' : 'none'" stroke="currentColor">
            <path d="M19 21l-7-5-7 5V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z"></path>
          </svg>
        </button>

        <!-- Share -->
        <button
          @click="handleShare"
          class="action-button"
          title="Share"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <circle cx="18" cy="5" r="3"></circle>
            <circle cx="6" cy="12" r="3"></circle>
            <circle cx="18" cy="19" r="3"></circle>
            <line x1="8.59" y1="13.51" x2="15.42" y2="17.49"></line>
            <line x1="15.41" y1="6.51" x2="8.59" y2="10.49"></line>
          </svg>
        </button>

        <!-- More Actions -->
        <div class="more-actions-container">
          <button
            @click="showMoreActions = !showMoreActions"
            class="action-button"
            title="More actions"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <circle cx="12" cy="12" r="1"></circle>
              <circle cx="19" cy="12" r="1"></circle>
              <circle cx="5" cy="12" r="1"></circle>
            </svg>
          </button>

          <!-- More Actions Menu -->
          <div v-if="showMoreActions" class="more-actions-menu">
            <button @click="handlePin" class="menu-item">
              <span class="menu-icon">📌</span>
              {{ isPinned ? 'Unpin' : 'Pin' }}
            </button>
            <button @click="handleHide" class="menu-item">
              <span class="menu-icon">👁️</span>
              Hide
            </button>
            <button @click="handleReport" class="menu-item danger">
              <span class="menu-icon">🚨</span>
              Report
            </button>
          </div>
        </div>
      </div>
    </footer>
  </article>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import type {
  FeedItem,
  ContentItem,
  ChatMessage,
  Event,
  Link,
  Person,
  Hangout,
  DirectMessage,
  LayoutMode
} from '../types/content'

// Import content-specific components
import ChatMessageContent from './content/ChatMessageContent.vue'
import EventContent from './content/EventContent.vue'
import LinkContent from './content/LinkContent.vue'
import PersonContent from './content/PersonContent.vue'
import HangoutContent from './content/HangoutContent.vue'
import DirectMessageContent from './content/DirectMessageContent.vue'

interface Props {
  item: FeedItem
  layout: LayoutMode
  interactive: boolean
  bookmarkedItems?: Set<string>
  pinnedItems?: Set<string>
}

const props = withDefaults(defineProps<Props>(), {
  layout: 'feed',
  interactive: true,
  bookmarkedItems: () => new Set(),
  pinnedItems: () => new Set(),
})

const emit = defineEmits<{
  react: [contentId: string, reaction: string]
  bookmark: [contentId: string, bookmarked: boolean]
  pin: [contentId: string, pinned: boolean]
  share: [contentId: string]
  reply: [contentId: string, replyText: string]
  vote: [contentId: string, direction: 'up' | 'down']
  rsvp: [eventId: string, status: 'going' | 'maybe' | 'not_going']
  follow: [userId: string, following: boolean]
  join: [hangoutId: string]
  directMessage: [userId: string]
  comment: [contentId: string]
  connect: [userId: string]
  hide: [contentId: string]
  report: [contentId: string, reason: string]
}>()

// Reactive state
const showReactionPicker = ref(false)
const showMoreActions = ref(false)

// Common reaction emojis
const commonReactions = ['👍', '❤️', '😂', '😮', '😢', '😡', '🎉', '🔥']

// Computed properties
const isBookmarked = computed(() => 
  props.bookmarkedItems?.has(props.item.content.id) ?? false
)

const isPinned = computed(() => 
  props.pinnedItems?.has(props.item.content.id) ?? false
)

// Content type icon mapping
const getTypeIcon = (type: string): string => {
  const icons = {
    chat: '💬',
    event: '📅',
    link: '🔗',
    person: '👤',
    hangout: '🎧',
    message: '💌',
  }
  return icons[type as keyof typeof icons] || '📄'
}

// Event handlers
const handleReaction = (reaction: string) => {
  showReactionPicker.value = false
  emit('react', props.item.content.id, reaction)
}

const handleBookmark = () => {
  emit('bookmark', props.item.content.id, !isBookmarked.value)
}

const handlePin = () => {
  emit('pin', props.item.content.id, !isPinned.value)
  showMoreActions.value = false
}

const handleShare = () => {
  emit('share', props.item.content.id)
}

const handleReply = (replyText: string) => {
  emit('reply', props.item.content.id, replyText)
}

const handleVote = (direction: 'up' | 'down') => {
  emit('vote', props.item.content.id, direction)
}

const handleRSVP = (status: 'going' | 'maybe' | 'not_going') => {
  emit('rsvp', props.item.content.id, status)
}

const handleFollow = (following: boolean) => {
  emit('follow', props.item.content.userId, following)
}

const handleJoinHangout = () => {
  emit('join', props.item.content.id)
}

const handleDirectMessage = () => {
  emit('directMessage', props.item.content.userId)
}

const handleComment = () => {
  emit('comment', props.item.content.id)
}

const handleConnect = () => {
  emit('connect', props.item.content.userId)
}

const handleHide = () => {
  emit('hide', props.item.content.id)
  showMoreActions.value = false
}

const handleReport = () => {
  // In a real app, this would open a report modal
  const reason = prompt('Please specify the reason for reporting this content:')
  if (reason) {
    emit('report', props.item.content.id, reason)
  }
  showMoreActions.value = false
}

// Click outside handler for dropdowns
const handleClickOutside = (event: Event) => {
  const target = event.target as HTMLElement
  
  if (!target.closest('.reactions-container')) {
    showReactionPicker.value = false
  }
  
  if (!target.closest('.more-actions-container')) {
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
/* Base Content Item Styles */
.content-item {
  position: relative;
  background: rgba(255, 255, 255, 0.02);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 16px;
  overflow: hidden;
  transition: all 0.3s cubic-bezier(0.4, 0, 0.2, 1);
  backdrop-filter: blur(10px);
}

.content-item:hover {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(255, 255, 255, 0.15);
  transform: translateY(-2px);
  box-shadow: 0 8px 25px rgba(0, 0, 0, 0.2);
}

.content-item.interactive {
  cursor: pointer;
}

/* Content State Modifiers */
.content-item.bookmarked {
  border-color: rgba(59, 130, 246, 0.3);
  box-shadow: 0 0 0 1px rgba(59, 130, 246, 0.1);
}

.content-item.pinned {
  border-color: rgba(245, 158, 11, 0.3);
  box-shadow: 0 0 0 1px rgba(245, 158, 11, 0.1);
}

.content-item.trending {
  border-color: rgba(239, 68, 68, 0.3);
  box-shadow: 0 0 0 1px rgba(239, 68, 68, 0.1);
}

.content-item.promoted {
  border-color: rgba(168, 85, 247, 0.3);
  box-shadow: 0 0 0 1px rgba(168, 85, 247, 0.1);
}

/* Content Type Indicator */
.content-indicator {
  position: absolute;
  top: 12px;
  right: 12px;
  display: flex;
  align-items: center;
  gap: 0.25rem;
  z-index: 10;
}

.content-type-icon {
  font-size: 0.9rem;
  opacity: 0.8;
}

.trending-badge,
.promoted-badge {
  font-size: 0.8rem;
  animation: pulse 2s infinite;
}

.trending-badge {
  color: #ef4444;
}

.promoted-badge {
  color: #a855f7;
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.7; }
}

/* Layout Variations */
.layout-feed {
  margin-bottom: 1rem;
}

.layout-grid {
  break-inside: avoid;
  margin-bottom: 1rem;
}

.layout-timeline {
  margin-bottom: 1.5rem;
  position: relative;
}

.layout-timeline::before {
  content: '';
  position: absolute;
  left: -20px;
  top: 0;
  bottom: 0;
  width: 2px;
  background: linear-gradient(to bottom, var(--accent-primary), transparent);
}

.layout-kanban {
  width: 300px;
  margin: 0.5rem;
  flex-shrink: 0;
}

/* Unknown Content Fallback */
.unknown-content {
  padding: 1rem;
  color: rgba(255, 255, 255, 0.7);
}

.unknown-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 1rem;
  padding-bottom: 0.5rem;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.unknown-type {
  font-weight: 600;
  color: #fbbf24;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  font-size: 0.8rem;
}

.unknown-id {
  font-family: monospace;
  font-size: 0.7rem;
  color: rgba(255, 255, 255, 0.5);
}

.unknown-data {
  background: rgba(0, 0, 0, 0.3);
  border-radius: 8px;
  padding: 1rem;
  font-size: 0.7rem;
  overflow: auto;
  max-height: 200px;
  color: rgba(255, 255, 255, 0.8);
}

/* Content Actions Footer */
.content-actions {
  padding: 0.75rem 1rem;
  border-top: 1px solid rgba(255, 255, 255, 0.05);
  background: rgba(0, 0, 0, 0.1);
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
}

.action-group {
  display: flex;
  align-items: center;
  gap: 0.75rem;
}

/* Reactions */
.reactions-container {
  position: relative;
  display: flex;
  align-items: center;
  gap: 0.25rem;
}

.reaction-button {
  display: flex;
  align-items: center;
  gap: 0.25rem;
  padding: 0.25rem 0.5rem;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  color: rgba(255, 255, 255, 0.8);
  cursor: pointer;
  font-size: 0.8rem;
  transition: all 0.2s ease;
}

.reaction-button:hover {
  background: rgba(255, 255, 255, 0.1);
  border-color: rgba(255, 255, 255, 0.2);
}

.reaction-button.user-reacted {
  background: rgba(var(--accent-primary-rgb), 0.2);
  border-color: var(--accent-primary);
  color: var(--accent-primary);
}

.reaction-emoji {
  font-size: 0.9rem;
  line-height: 1;
}

.reaction-count {
  font-weight: 500;
  font-size: 0.75rem;
  color: inherit;
}

.add-reaction {
  padding: 0.25rem;
  background: none;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  transition: all 0.2s ease;
}

.add-reaction:hover {
  color: rgba(255, 255, 255, 0.8);
  border-color: rgba(255, 255, 255, 0.2);
  background: rgba(255, 255, 255, 0.05);
}

.add-reaction svg {
  width: 16px;
  height: 16px;
}

/* Reaction Picker */
.reaction-picker {
  position: absolute;
  bottom: 100%;
  left: 0;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  padding: 0.5rem;
  margin-bottom: 0.5rem;
  display: flex;
  gap: 0.25rem;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
  z-index: 100;
}

.picker-emoji {
  background: none;
  border: none;
  font-size: 1.2rem;
  padding: 0.25rem;
  cursor: pointer;
  border-radius: 6px;
  transition: all 0.2s ease;
}

.picker-emoji:hover {
  background: rgba(255, 255, 255, 0.1);
  transform: scale(1.1);
}

/* Action Buttons */
.action-button {
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  padding: 0.5rem;
  border-radius: 8px;
  transition: all 0.2s ease;
  position: relative;
}

.action-button:hover {
  color: rgba(255, 255, 255, 0.8);
  background: rgba(255, 255, 255, 0.05);
}

.action-button.active {
  color: var(--accent-primary);
}

.action-button svg {
  width: 18px;
  height: 18px;
}

/* More Actions */
.more-actions-container {
  position: relative;
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

/* Responsive Design */
@media (max-width: 768px) {
  .content-actions {
    padding: 0.5rem 0.75rem;
    gap: 0.5rem;
  }
  
  .action-group {
    gap: 0.5rem;
  }
  
  .reaction-button {
    padding: 0.25rem;
    font-size: 0.75rem;
  }
  
  .reaction-count {
    display: none;
  }
  
  .action-button {
    padding: 0.375rem;
  }
  
  .action-button svg {
    width: 16px;
    height: 16px;
  }
}

@media (max-width: 480px) {
  .content-item {
    border-radius: 12px;
  }
  
  .content-actions {
    flex-direction: column;
    align-items: stretch;
    gap: 0.5rem;
  }
  
  .action-group {
    justify-content: center;
  }
  
  .secondary-actions {
    border-top: 1px solid rgba(255, 255, 255, 0.05);
    padding-top: 0.5rem;
  }
}
</style>
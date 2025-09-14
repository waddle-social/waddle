<template>
  <div class="link-content">
    <!-- Link Header -->
    <header class="link-header">
      <div class="poster-info">
        <img 
          v-if="link.avatar" 
          :src="link.avatar" 
          :alt="link.username"
          class="poster-avatar"
        />
        <div v-else class="poster-avatar-placeholder">
          {{ link.username.charAt(0).toUpperCase() }}
        </div>
        <div class="poster-details">
          <span class="poster-name">{{ link.username }}</span>
          <span class="post-time">{{ formatTime(link.timestamp) }}</span>
        </div>
      </div>
      
      <div class="vote-section">
        <div class="vote-score" :class="{ 'positive': link.votes > 0, 'negative': link.votes < 0 }">
          {{ formatVotes(link.votes) }}
        </div>
      </div>
    </header>

    <!-- Link Preview -->
    <main class="link-body">
      <div class="link-preview">
        <div v-if="link.thumbnail" class="preview-image">
          <img :src="link.thumbnail" :alt="link.title" />
        </div>
        
        <div class="preview-content">
          <div class="link-domain">{{ link.domain }}</div>
          <h3 class="link-title">
            <a :href="link.url" target="_blank" rel="noopener noreferrer">
              {{ link.title }}
            </a>
          </h3>
          <p v-if="link.description" class="link-description">
            {{ link.description }}
          </p>
          
          <div class="link-meta">
            <span class="comment-count">{{ link.commentCount }} comments</span>
            <span v-if="link.isNSFW" class="nsfw-tag">NSFW</span>
          </div>
        </div>
      </div>
    </main>

    <!-- Voting & Actions -->
    <footer v-if="interactive" class="link-actions">
      <div class="voting-controls">
        <button 
          @click="handleVote('up')"
          :class="['vote-btn', 'upvote', { 'active': link.userVote === 'up' }]"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M18 15l-6-6-6 6"></path>
          </svg>
          Upvote
        </button>
        
        <button 
          @click="handleVote('down')"
          :class="['vote-btn', 'downvote', { 'active': link.userVote === 'down' }]"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M6 9l6 6 6-6"></path>
          </svg>
          Downvote
        </button>
      </div>

      <div class="link-secondary-actions">
        <button @click="handleComment" class="action-btn">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"></path>
          </svg>
          Comment
        </button>
        
        <button @click="handleVisitLink" class="action-btn visit-btn">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"></path>
            <polyline points="15,3 21,3 21,9"></polyline>
            <line x1="10" y1="14" x2="21" y2="3"></line>
          </svg>
          Visit
        </button>
      </div>
    </footer>
  </div>
</template>

<script setup lang="ts">
import type { Link, LayoutMode } from '../../types/content'

interface Props {
  link: Link
  layout: LayoutMode
  interactive: boolean
}

const props = defineProps<Props>()

const emit = defineEmits<{
  vote: [direction: 'up' | 'down']
  comment: []
  bookmark: []
  share: []
}>()

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

const formatVotes = (votes: number) => {
  if (votes === 0) return '0'
  if (Math.abs(votes) < 1000) return votes.toString()
  if (Math.abs(votes) < 1000000) return (votes / 1000).toFixed(1) + 'k'
  return (votes / 1000000).toFixed(1) + 'M'
}

const handleVote = (direction: 'up' | 'down') => {
  emit('vote', direction)
}

const handleComment = () => {
  emit('comment')
}

const handleVisitLink = () => {
  window.open(props.link.url, '_blank', 'noopener,noreferrer')
}
</script>

<style scoped>
.link-content {
  padding: 1rem;
  color: white;
}

.link-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 1rem;
}

.poster-info {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  flex: 1;
}

.poster-avatar {
  width: 32px;
  height: 32px;
  border-radius: 50%;
  object-fit: cover;
}

.poster-avatar-placeholder {
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

.poster-details {
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
}

.poster-name {
  font-weight: 600;
  font-size: 0.9rem;
  color: white;
}

.post-time {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
}

.vote-section {
  flex-shrink: 0;
}

.vote-score {
  font-weight: 700;
  font-size: 1rem;
  color: rgba(255, 255, 255, 0.8);
  padding: 0.25rem 0.5rem;
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.05);
}

.vote-score.positive {
  color: #22c55e;
  background: rgba(34, 197, 94, 0.1);
}

.vote-score.negative {
  color: #ef4444;
  background: rgba(239, 68, 68, 0.1);
}

.link-preview {
  display: flex;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  overflow: hidden;
  transition: all 0.2s ease;
  margin-bottom: 1rem;
}

.link-preview:hover {
  background: rgba(255, 255, 255, 0.05);
  border-color: rgba(255, 255, 255, 0.15);
}

.preview-image {
  width: 120px;
  height: 120px;
  flex-shrink: 0;
  overflow: hidden;
}

.preview-image img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.preview-content {
  flex: 1;
  padding: 1rem;
  display: flex;
  flex-direction: column;
  justify-content: space-between;
}

.link-domain {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
  text-transform: uppercase;
  letter-spacing: 0.025em;
  margin-bottom: 0.5rem;
}

.link-title {
  margin: 0 0 0.5rem 0;
  font-size: 1.1rem;
  font-weight: 600;
  line-height: 1.3;
}

.link-title a {
  color: white;
  text-decoration: none;
  transition: color 0.2s ease;
}

.link-title a:hover {
  color: var(--accent-primary);
}

.link-description {
  color: rgba(255, 255, 255, 0.7);
  font-size: 0.9rem;
  line-height: 1.4;
  margin-bottom: 0.75rem;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.link-meta {
  display: flex;
  align-items: center;
  gap: 1rem;
  font-size: 0.8rem;
}

.comment-count {
  color: rgba(255, 255, 255, 0.6);
}

.nsfw-tag {
  background: rgba(239, 68, 68, 0.2);
  color: #ef4444;
  padding: 0.125rem 0.5rem;
  border-radius: 8px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.025em;
  font-size: 0.7rem;
}

.link-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  padding-top: 0.75rem;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}

.voting-controls {
  display: flex;
  gap: 0.5rem;
}

.vote-btn {
  display: flex;
  align-items: center;
  gap: 0.375rem;
  padding: 0.5rem 0.75rem;
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.05);
  color: rgba(255, 255, 255, 0.7);
  cursor: pointer;
  font-size: 0.8rem;
  font-weight: 500;
  transition: all 0.2s ease;
}

.vote-btn:hover {
  background: rgba(255, 255, 255, 0.1);
  color: white;
}

.vote-btn svg {
  width: 14px;
  height: 14px;
}

.upvote.active {
  background: rgba(34, 197, 94, 0.2);
  border-color: #22c55e;
  color: #22c55e;
}

.downvote.active {
  background: rgba(239, 68, 68, 0.2);
  border-color: #ef4444;
  color: #ef4444;
}

.link-secondary-actions {
  display: flex;
  gap: 0.5rem;
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

.visit-btn:hover {
  color: var(--accent-primary);
  background: rgba(var(--accent-primary-rgb), 0.1);
}

@media (max-width: 640px) {
  .link-preview {
    flex-direction: column;
  }
  
  .preview-image {
    width: 100%;
    height: 160px;
  }
  
  .link-actions {
    flex-direction: column;
    gap: 0.75rem;
    align-items: stretch;
  }
  
  .voting-controls {
    justify-content: center;
  }
  
  .link-secondary-actions {
    justify-content: center;
  }
}
</style>
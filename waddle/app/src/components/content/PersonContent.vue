<template>
  <div class="person-content">
    <div class="person-card">
      <!-- Person Header -->
      <header class="person-header">
        <div class="avatar-section">
          <img 
            v-if="person.avatar" 
            :src="person.avatar" 
            :alt="person.displayName"
            class="person-avatar"
          />
          <div v-else class="avatar-placeholder">
            {{ person.displayName.charAt(0).toUpperCase() }}
          </div>
          
          <div v-if="person.isOnline" class="online-indicator"></div>
        </div>
        
        <div class="person-info">
          <h3 class="person-name">{{ person.displayName }}</h3>
          <div class="person-username">@{{ person.username }}</div>
          <div class="person-status">
            {{ person.isOnline ? 'Online' : `Last seen ${formatTime(person.lastSeen || 0)}` }}
          </div>
        </div>
      </header>

      <!-- Person Stats -->
      <div class="person-stats">
        <div class="stat-item">
          <span class="stat-value">{{ formatNumber(person.followerCount) }}</span>
          <span class="stat-label">Followers</span>
        </div>
        <div class="stat-item">
          <span class="stat-value">{{ formatNumber(person.followingCount) }}</span>
          <span class="stat-label">Following</span>
        </div>
        <div v-if="person.mutualConnections > 0" class="stat-item">
          <span class="stat-value">{{ person.mutualConnections }}</span>
          <span class="stat-label">Mutual</span>
        </div>
      </div>

      <!-- Person Bio -->
      <div v-if="person.bio" class="person-bio">
        {{ person.bio }}
      </div>

      <!-- Person Details -->
      <div class="person-details">
        <div v-if="person.location" class="detail-item">
          <svg class="detail-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z"></path>
            <circle cx="12" cy="10" r="3"></circle>
          </svg>
          <span>{{ person.location }}</span>
        </div>
        
        <div v-if="person.website" class="detail-item">
          <svg class="detail-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"></path>
            <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"></path>
          </svg>
          <a :href="person.website" target="_blank" rel="noopener noreferrer">
            {{ formatWebsiteUrl(person.website) }}
          </a>
        </div>
        
        <div class="detail-item">
          <svg class="detail-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <rect x="3" y="4" width="18" height="18" rx="2" ry="2"></rect>
            <line x1="16" y1="2" x2="16" y2="6"></line>
            <line x1="8" y1="2" x2="8" y2="6"></line>
            <line x1="3" y1="10" x2="21" y2="10"></line>
          </svg>
          <span>Joined {{ formatJoinDate(person.joinedAt) }}</span>
        </div>
      </div>

      <!-- Skills & Interests -->
      <div v-if="person.skills && person.skills.length > 0" class="person-tags">
        <div class="tags-label">Skills:</div>
        <div class="tags-container">
          <span 
            v-for="skill in person.skills.slice(0, 5)" 
            :key="skill"
            class="tag skill-tag"
          >
            {{ skill }}
          </span>
          <span v-if="person.skills.length > 5" class="tag more-tag">
            +{{ person.skills.length - 5 }} more
          </span>
        </div>
      </div>

      <div v-if="person.interests && person.interests.length > 0" class="person-tags">
        <div class="tags-label">Interests:</div>
        <div class="tags-container">
          <span 
            v-for="interest in person.interests.slice(0, 5)" 
            :key="interest"
            class="tag interest-tag"
          >
            {{ interest }}
          </span>
          <span v-if="person.interests.length > 5" class="tag more-tag">
            +{{ person.interests.length - 5 }} more
          </span>
        </div>
      </div>

      <!-- Actions -->
      <footer v-if="interactive" class="person-actions">
        <button 
          @click="handleFollow"
          :class="['action-btn', 'follow-btn', { 'following': person.isFollowing }]"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <g v-if="person.isFollowing">
              <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"></path>
              <circle cx="9" cy="7" r="4"></circle>
              <line x1="22" y1="11" x2="16" y2="17"></line>
              <line x1="16" y1="11" x2="22" y2="17"></line>
            </g>
            <g v-else>
              <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"></path>
              <circle cx="9" cy="7" r="4"></circle>
              <line x1="19" y1="8" x2="19" y2="14"></line>
              <line x1="22" y1="11" x2="16" y2="11"></line>
            </g>
          </svg>
          {{ person.isFollowing ? 'Unfollow' : 'Follow' }}
        </button>
        
        <button @click="handleMessage" class="action-btn message-btn">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"></path>
          </svg>
          Message
        </button>
        
        <button @click="handleConnect" class="action-btn connect-btn">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"></path>
            <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"></path>
          </svg>
          Connect
        </button>
      </footer>
    </div>
  </div>
</template>

<script setup lang="ts">
import type { Person, LayoutMode } from '../../types/content'

interface Props {
  person: Person
  layout: LayoutMode
  interactive: boolean
}

const props = defineProps<Props>()

const emit = defineEmits<{
  follow: [following: boolean]
  message: []
  connect: []
}>()

const formatTime = (timestamp: number) => {
  if (!timestamp) return 'never'
  
  const now = Date.now()
  const diff = now - timestamp
  const minutes = Math.floor(diff / 60000)
  const hours = Math.floor(minutes / 60)
  const days = Math.floor(hours / 24)
  const months = Math.floor(days / 30)
  
  if (months > 0) return `${months}mo ago`
  if (days > 0) return `${days}d ago`
  if (hours > 0) return `${hours}h ago`
  if (minutes > 0) return `${minutes}m ago`
  return 'Just now'
}

const formatNumber = (num: number) => {
  if (num < 1000) return num.toString()
  if (num < 1000000) return (num / 1000).toFixed(1) + 'k'
  return (num / 1000000).toFixed(1) + 'M'
}

const formatJoinDate = (timestamp: number) => {
  const date = new Date(timestamp)
  return date.toLocaleDateString(undefined, { 
    year: 'numeric', 
    month: 'short' 
  })
}

const formatWebsiteUrl = (url: string) => {
  try {
    const domain = new URL(url).hostname
    return domain.replace('www.', '')
  } catch {
    return url
  }
}

const handleFollow = () => {
  emit('follow', !props.person.isFollowing)
}

const handleMessage = () => {
  emit('message')
}

const handleConnect = () => {
  emit('connect')
}
</script>

<style scoped>
.person-content {
  padding: 1rem;
  color: white;
}

.person-card {
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.person-header {
  display: flex;
  align-items: flex-start;
  gap: 1rem;
}

.avatar-section {
  position: relative;
  flex-shrink: 0;
}

.person-avatar {
  width: 60px;
  height: 60px;
  border-radius: 50%;
  object-fit: cover;
  border: 2px solid rgba(255, 255, 255, 0.1);
}

.avatar-placeholder {
  width: 60px;
  height: 60px;
  background: var(--accent-primary);
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  font-weight: 700;
  font-size: 1.5rem;
  border: 2px solid rgba(255, 255, 255, 0.1);
}

.online-indicator {
  position: absolute;
  bottom: 2px;
  right: 2px;
  width: 14px;
  height: 14px;
  background: #22c55e;
  border: 2px solid rgba(0, 0, 0, 0.8);
  border-radius: 50%;
}

.person-info {
  flex: 1;
  min-width: 0;
}

.person-name {
  font-size: 1.25rem;
  font-weight: 700;
  color: white;
  margin: 0 0 0.25rem 0;
  word-wrap: break-word;
}

.person-username {
  font-size: 0.9rem;
  color: rgba(255, 255, 255, 0.6);
  margin-bottom: 0.25rem;
}

.person-status {
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.5);
}

.person-stats {
  display: flex;
  gap: 1.5rem;
  padding: 0.75rem 0;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.stat-item {
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
}

.stat-value {
  font-size: 1.1rem;
  font-weight: 700;
  color: white;
}

.stat-label {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.6);
  text-transform: uppercase;
  letter-spacing: 0.025em;
  margin-top: 0.125rem;
}

.person-bio {
  color: rgba(255, 255, 255, 0.8);
  line-height: 1.5;
  font-size: 0.9rem;
}

.person-details {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.detail-item {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.7);
}

.detail-icon {
  width: 14px;
  height: 14px;
  color: rgba(255, 255, 255, 0.5);
  flex-shrink: 0;
}

.detail-item a {
  color: var(--accent-primary);
  text-decoration: none;
}

.detail-item a:hover {
  text-decoration: underline;
}

.person-tags {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.tags-label {
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.6);
  font-weight: 600;
}

.tags-container {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.tag {
  padding: 0.25rem 0.75rem;
  border-radius: 12px;
  font-size: 0.75rem;
  font-weight: 500;
  text-transform: capitalize;
}

.skill-tag {
  background: rgba(59, 130, 246, 0.2);
  color: #3b82f6;
}

.interest-tag {
  background: rgba(168, 85, 247, 0.2);
  color: #a855f7;
}

.more-tag {
  background: rgba(255, 255, 255, 0.1);
  color: rgba(255, 255, 255, 0.7);
  font-style: italic;
}

.person-actions {
  display: flex;
  gap: 0.5rem;
  padding-top: 0.75rem;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  flex-wrap: wrap;
}

.action-btn {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.5rem 1rem;
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.05);
  color: rgba(255, 255, 255, 0.8);
  cursor: pointer;
  font-size: 0.8rem;
  font-weight: 500;
  transition: all 0.2s ease;
  flex: 1;
  justify-content: center;
  min-width: 0;
}

.action-btn:hover {
  background: rgba(255, 255, 255, 0.1);
  border-color: rgba(255, 255, 255, 0.3);
  color: white;
}

.action-btn svg {
  width: 16px;
  height: 16px;
  flex-shrink: 0;
}

.follow-btn.following {
  background: rgba(239, 68, 68, 0.1);
  border-color: #ef4444;
  color: #ef4444;
}

.follow-btn.following:hover {
  background: rgba(239, 68, 68, 0.2);
}

.follow-btn:not(.following) {
  background: rgba(34, 197, 94, 0.1);
  border-color: #22c55e;
  color: #22c55e;
}

.follow-btn:not(.following):hover {
  background: rgba(34, 197, 94, 0.2);
}

.message-btn:hover {
  background: rgba(var(--accent-primary-rgb), 0.1);
  border-color: var(--accent-primary);
  color: var(--accent-primary);
}

.connect-btn:hover {
  background: rgba(245, 158, 11, 0.1);
  border-color: #f59e0b;
  color: #f59e0b;
}

@media (max-width: 480px) {
  .person-header {
    flex-direction: column;
    align-items: center;
    text-align: center;
  }
  
  .person-stats {
    justify-content: center;
  }
  
  .person-actions {
    flex-direction: column;
  }
  
  .action-btn {
    flex: none;
  }
}
</style>
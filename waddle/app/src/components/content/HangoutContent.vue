<template>
  <div class="hangout-content">
    <!-- Hangout Header -->
    <header class="hangout-header">
      <div class="host-info">
        <img 
          v-if="hangout.avatar" 
          :src="hangout.avatar" 
          :alt="hangout.username"
          class="host-avatar"
        />
        <div v-else class="host-avatar-placeholder">
          {{ hangout.username.charAt(0).toUpperCase() }}
        </div>
        <div class="host-details">
          <span class="host-name">{{ hangout.username }}</span>
          <span class="created-time">{{ formatTime(hangout.timestamp) }}</span>
        </div>
      </div>
      
      <div class="hangout-status">
        <span :class="['status-badge', hangout.isLive ? 'live' : 'scheduled']">
          {{ hangout.isLive ? '🔴 LIVE' : '📅 Scheduled' }}
        </span>
      </div>
    </header>

    <!-- Hangout Preview -->
    <main class="hangout-body">
      <div class="hangout-preview">
        <div class="preview-thumbnail">
          <img 
            v-if="hangout.thumbnailUrl" 
            :src="hangout.thumbnailUrl" 
            :alt="hangout.title"
            class="thumbnail-image"
          />
          <div v-else class="thumbnail-placeholder">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <g v-if="hangout.hangoutType === 'voice'">
                <path d="M12 1l3 3h-6l3-3zM12 13c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zM20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"></path>
              </g>
              <g v-else-if="hangout.hangoutType === 'video'">
                <polygon points="23 7 16 12 23 17 23 7"></polygon>
                <rect x="1" y="5" width="15" height="14" rx="2" ry="2"></rect>
              </g>
              <g v-else-if="hangout.hangoutType === 'stream'">
                <circle cx="12" cy="12" r="3"></circle>
                <path d="M12 1v6m0 6v6m11-7h-6m-6 0H1"></path>
              </g>
              <g v-else>
                <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
                <line x1="8" y1="21" x2="16" y2="21"></line>
                <line x1="12" y1="17" x2="12" y2="21"></line>
              </g>
            </svg>
          </div>
          
          <!-- Type Indicator -->
          <div class="hangout-type-badge">
            {{ getTypeIcon(hangout.hangoutType) }} {{ getTypeLabel(hangout.hangoutType) }}
          </div>
          
          <!-- Duration for streams -->
          <div v-if="hangout.duration && hangout.isLive" class="duration-badge">
            {{ formatDuration(hangout.duration) }}
          </div>
        </div>
        
        <div class="hangout-info">
          <h3 class="hangout-title">{{ hangout.title }}</h3>
          
          <p v-if="hangout.description" class="hangout-description">
            {{ hangout.description }}
          </p>
          
          <!-- Participant Info -->
          <div class="participant-info">
            <div class="participant-count">
              <svg class="participant-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
                <circle cx="9" cy="7" r="4"></circle>
                <path d="M23 21v-2a4 4 0 0 0-3-3.87"></path>
                <path d="M16 3.13a4 4 0 0 1 0 7.75"></path>
              </svg>
              <span>
                {{ hangout.participantCount }}{{ hangout.maxParticipants ? `/${hangout.maxParticipants}` : '' }}
                {{ hangout.participantCount === 1 ? 'person' : 'people' }}
              </span>
            </div>
            
            <div v-if="hangout.maxParticipants" class="capacity-bar">
              <div 
                class="capacity-fill" 
                :style="{ width: `${Math.min(100, (hangout.participantCount / hangout.maxParticipants) * 100)}%` }"
              ></div>
            </div>
          </div>
          
          <!-- Scheduled Time -->
          <div v-if="hangout.scheduledFor && !hangout.isLive" class="scheduled-time">
            <svg class="schedule-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <circle cx="12" cy="12" r="10"></circle>
              <polyline points="12,6 12,12 16,14"></polyline>
            </svg>
            <span>Starts {{ formatScheduledTime(hangout.scheduledFor) }}</span>
          </div>
          
          <!-- Privacy & Access -->
          <div class="hangout-access">
            <div class="access-info">
              <svg class="access-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                <g v-if="hangout.isPublic">
                  <rect x="3" y="11" width="18" height="11" rx="2" ry="2"></rect>
                  <circle cx="12" cy="16" r="1"></circle>
                  <path d="M7 11V7a5 5 0 0 1 10 0v4"></path>
                </g>
                <g v-else>
                  <rect x="3" y="11" width="18" height="10" rx="2" ry="2"></rect>
                  <circle cx="12" cy="16" r="1"></circle>
                  <path d="M7 11V7a5 5 0 0 1 10 0v4"></path>
                </g>
              </svg>
              <span>{{ hangout.isPublic ? 'Public' : 'Private' }} hangout</span>
            </div>
            
            <div v-if="hangout.requiresApproval" class="approval-required">
              <svg class="approval-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                <polyline points="9,11 12,14 22,4"></polyline>
                <path d="M21 12c0 4.97-4.03 9-9 9s-9-4.03-9-9 4.03-9 9-9c1.67 0 3.23.46 4.57 1.26"></path>
              </svg>
              <span>Approval required</span>
            </div>
          </div>
        </div>
      </div>
    </main>

    <!-- Join Actions -->
    <footer v-if="interactive" class="hangout-actions">
      <button 
        @click="handleJoin"
        :class="['join-btn', { 'live': hangout.isLive, 'full': isAtCapacity }]"
        :disabled="isAtCapacity"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <g v-if="hangout.hangoutType === 'voice'">
            <path d="M12 1l3 3h-6l3-3zM12 13c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4z"></path>
          </g>
          <g v-else-if="hangout.hangoutType === 'video'">
            <polygon points="23 7 16 12 23 17 23 7"></polygon>
            <rect x="1" y="5" width="15" height="14" rx="2" ry="2"></rect>
          </g>
          <g v-else>
            <circle cx="12" cy="12" r="3"></circle>
            <path d="M12 1v6m0 6v6m11-7h-6m-6 0H1"></path>
          </g>
        </svg>
        {{ getJoinButtonText() }}
      </button>
      
      <div class="secondary-actions">
        <button @click="handleNotifyMe" class="action-btn notify-btn">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9"></path>
            <path d="M13.73 21a2 2 0 0 1-3.46 0"></path>
          </svg>
          Notify Me
        </button>
        
        <button v-if="hangout.streamUrl" @click="handleCopyStreamUrl" class="action-btn copy-btn">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
          </svg>
          Copy Link
        </button>
      </div>
    </footer>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { Hangout, LayoutMode } from '../../types/content'

interface Props {
  hangout: Hangout
  layout: LayoutMode
  interactive: boolean
}

const props = defineProps<Props>()

const emit = defineEmits<{
  join: []
  bookmark: []
  share: []
}>()

const isAtCapacity = computed(() => {
  return props.hangout.maxParticipants && 
         props.hangout.participantCount >= props.hangout.maxParticipants
})

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

const formatScheduledTime = (timestamp: number) => {
  const date = new Date(timestamp)
  const now = new Date()
  const isToday = date.toDateString() === now.toDateString()
  const isTomorrow = date.toDateString() === new Date(now.getTime() + 86400000).toDateString()
  
  if (isToday) {
    return `today at ${date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`
  } else if (isTomorrow) {
    return `tomorrow at ${date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`
  } else {
    return date.toLocaleString([], { 
      weekday: 'short',
      month: 'short', 
      day: 'numeric',
      hour: '2-digit', 
      minute: '2-digit' 
    })
  }
}

const formatDuration = (seconds: number) => {
  const hours = Math.floor(seconds / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  
  if (hours > 0) {
    return `${hours}:${minutes.toString().padStart(2, '0')}:${(seconds % 60).toString().padStart(2, '0')}`
  }
  return `${minutes}:${(seconds % 60).toString().padStart(2, '0')}`
}

const getTypeIcon = (type: string) => {
  const icons = {
    voice: '🎙️',
    video: '📹',
    stream: '📡',
    watch_party: '🍿'
  }
  return icons[type as keyof typeof icons] || '🎧'
}

const getTypeLabel = (type: string) => {
  const labels = {
    voice: 'Voice Chat',
    video: 'Video Call',
    stream: 'Live Stream',
    watch_party: 'Watch Party'
  }
  return labels[type as keyof typeof labels] || 'Hangout'
}

const getJoinButtonText = () => {
  if (isAtCapacity.value) return 'Full'
  if (!props.hangout.isLive && props.hangout.scheduledFor) return 'Set Reminder'
  
  const labels = {
    voice: 'Join Voice',
    video: 'Join Video',
    stream: 'Watch Stream',
    watch_party: 'Join Watch Party'
  }
  return labels[props.hangout.hangoutType as keyof typeof labels] || 'Join Hangout'
}

const handleJoin = () => {
  if (!isAtCapacity.value) {
    emit('join')
  }
}

const handleNotifyMe = () => {
  // In a real app, this would set up notifications
  alert('You\'ll be notified when this hangout starts!')
}

const handleCopyStreamUrl = () => {
  if (props.hangout.streamUrl) {
    navigator.clipboard?.writeText(props.hangout.streamUrl)
    alert('Stream URL copied to clipboard!')
  }
}
</script>

<style scoped>
.hangout-content {
  padding: 1rem;
  color: white;
}

.hangout-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 1rem;
}

.host-info {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  flex: 1;
}

.host-avatar {
  width: 32px;
  height: 32px;
  border-radius: 50%;
  object-fit: cover;
}

.host-avatar-placeholder {
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

.host-details {
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
}

.host-name {
  font-weight: 600;
  font-size: 0.9rem;
  color: white;
}

.created-time {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
}

.status-badge {
  padding: 0.25rem 0.75rem;
  border-radius: 12px;
  font-size: 0.7rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.025em;
  animation: pulse 2s infinite;
}

.status-badge.live {
  background: rgba(239, 68, 68, 0.2);
  color: #ef4444;
}

.status-badge.scheduled {
  background: rgba(59, 130, 246, 0.2);
  color: #3b82f6;
  animation: none;
}

.hangout-preview {
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.preview-thumbnail {
  position: relative;
  width: 100%;
  height: 200px;
  border-radius: 12px;
  overflow: hidden;
  background: rgba(0, 0, 0, 0.4);
}

.thumbnail-image {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.thumbnail-placeholder {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, rgba(var(--accent-primary-rgb), 0.2) 0%, rgba(var(--accent-primary-rgb), 0.05) 100%);
}

.thumbnail-placeholder svg {
  width: 48px;
  height: 48px;
  color: rgba(255, 255, 255, 0.4);
}

.hangout-type-badge {
  position: absolute;
  top: 12px;
  left: 12px;
  background: rgba(0, 0, 0, 0.8);
  backdrop-filter: blur(8px);
  color: white;
  padding: 0.375rem 0.75rem;
  border-radius: 8px;
  font-size: 0.75rem;
  font-weight: 500;
  display: flex;
  align-items: center;
  gap: 0.375rem;
}

.duration-badge {
  position: absolute;
  bottom: 12px;
  right: 12px;
  background: rgba(239, 68, 68, 0.9);
  color: white;
  padding: 0.25rem 0.75rem;
  border-radius: 6px;
  font-size: 0.75rem;
  font-weight: 600;
  font-family: monospace;
}

.hangout-info {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.hangout-title {
  font-size: 1.25rem;
  font-weight: 700;
  color: white;
  margin: 0;
  line-height: 1.3;
}

.hangout-description {
  color: rgba(255, 255, 255, 0.8);
  line-height: 1.5;
  font-size: 0.9rem;
  margin: 0;
}

.participant-info {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.participant-count {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.participant-icon {
  width: 16px;
  height: 16px;
  color: var(--accent-primary);
}

.participant-count span {
  font-size: 0.9rem;
  color: rgba(255, 255, 255, 0.8);
  font-weight: 500;
}

.capacity-bar {
  height: 4px;
  background: rgba(255, 255, 255, 0.1);
  border-radius: 2px;
  overflow: hidden;
}

.capacity-fill {
  height: 100%;
  background: var(--accent-primary);
  transition: width 0.3s ease;
}

.scheduled-time {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.schedule-icon {
  width: 16px;
  height: 16px;
  color: var(--accent-primary);
}

.scheduled-time span {
  font-size: 0.9rem;
  color: rgba(255, 255, 255, 0.8);
  font-weight: 500;
}

.hangout-access {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.access-info,
.approval-required {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.7);
}

.access-icon,
.approval-icon {
  width: 14px;
  height: 14px;
  color: rgba(255, 255, 255, 0.5);
}

.hangout-actions {
  padding-top: 1rem;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.join-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  padding: 0.75rem 1.5rem;
  background: var(--accent-primary);
  border: none;
  border-radius: 12px;
  color: white;
  font-size: 0.9rem;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.2s ease;
  width: 100%;
}

.join-btn:hover:not(:disabled) {
  background: var(--accent-primary-dark);
  transform: translateY(-1px);
  box-shadow: 0 4px 12px rgba(var(--accent-primary-rgb), 0.3);
}

.join-btn:disabled {
  background: rgba(255, 255, 255, 0.1);
  color: rgba(255, 255, 255, 0.5);
  cursor: not-allowed;
}

.join-btn.live {
  animation: pulse 2s infinite;
}

.join-btn svg {
  width: 18px;
  height: 18px;
}

.secondary-actions {
  display: flex;
  gap: 0.5rem;
  justify-content: center;
}

.action-btn {
  display: flex;
  align-items: center;
  gap: 0.375rem;
  background: none;
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 8px;
  color: rgba(255, 255, 255, 0.7);
  cursor: pointer;
  padding: 0.5rem 0.75rem;
  font-size: 0.8rem;
  font-weight: 500;
  transition: all 0.2s ease;
}

.action-btn:hover {
  color: white;
  border-color: rgba(255, 255, 255, 0.4);
  background: rgba(255, 255, 255, 0.05);
}

.action-btn svg {
  width: 14px;
  height: 14px;
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.7; }
}

@media (max-width: 640px) {
  .preview-thumbnail {
    height: 160px;
  }
  
  .hangout-header {
    flex-direction: column;
    gap: 0.75rem;
    align-items: flex-start;
  }
  
  .secondary-actions {
    flex-direction: column;
  }
}
</style>
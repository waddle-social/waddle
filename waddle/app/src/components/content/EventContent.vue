<template>
  <div class="event-content">
    <!-- Event Header -->
    <header class="event-header">
      <div class="event-meta">
        <div class="organizer-info">
          <img 
            v-if="event.avatar" 
            :src="event.avatar" 
            :alt="event.username"
            class="organizer-avatar"
          />
          <div v-else class="organizer-avatar-placeholder">
            {{ event.username.charAt(0).toUpperCase() }}
          </div>
          <div class="organizer-details">
            <span class="organizer-name">{{ event.username }}</span>
            <span class="created-time">{{ formatTime(event.timestamp) }}</span>
          </div>
        </div>
        
        <div class="event-status">
          <span :class="['rsvp-badge', `rsvp-${event.rsvpStatus}`]">
            {{ getRSVPStatusText(event.rsvpStatus) }}
          </span>
        </div>
      </div>
    </header>

    <!-- Event Details -->
    <main class="event-body">
      <div class="event-title">{{ event.title }}</div>
      
      <div class="event-description" v-if="event.description">
        {{ event.description }}
      </div>

      <!-- Date & Time -->
      <div class="event-datetime">
        <div class="datetime-item">
          <svg class="datetime-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <rect x="3" y="4" width="18" height="18" rx="2" ry="2"></rect>
            <line x1="16" y1="2" x2="16" y2="6"></line>
            <line x1="8" y1="2" x2="8" y2="6"></line>
            <line x1="3" y1="10" x2="21" y2="10"></line>
          </svg>
          <div class="datetime-details">
            <div class="start-time">
              {{ formatEventDate(event.startTime) }}
            </div>
            <div class="end-time">
              → {{ formatEventDate(event.endTime) }}
            </div>
          </div>
        </div>
      </div>

      <!-- Location -->
      <div v-if="event.location" class="event-location">
        <svg class="location-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z"></path>
          <circle cx="12" cy="10" r="3"></circle>
        </svg>
        <div class="location-details">
          <div class="location-type">
            {{ event.location.type === 'virtual' ? '🌐 Virtual Event' : '📍 In-Person' }}
          </div>
          <div class="location-address">
            {{ event.location.address || event.location.virtualUrl || 'Location TBD' }}
          </div>
        </div>
      </div>

      <!-- Attendees -->
      <div class="event-attendees">
        <div class="attendee-count">
          <svg class="attendee-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
            <circle cx="9" cy="7" r="4"></circle>
            <path d="M23 21v-2a4 4 0 0 0-3-3.87"></path>
            <path d="M16 3.13a4 4 0 0 1 0 7.75"></path>
          </svg>
          <span class="count-text">
            {{ event.attendeeCount }} 
            {{ event.attendeeCount === 1 ? 'person' : 'people' }}
            {{ event.maxAttendees ? `/ ${event.maxAttendees}` : '' }}
            going
          </span>
        </div>
        
        <div v-if="event.maxAttendees" class="capacity-bar">
          <div 
            class="capacity-fill" 
            :style="{ width: `${Math.min(100, (event.attendeeCount / event.maxAttendees) * 100)}%` }"
          ></div>
        </div>
      </div>

      <!-- Recurring Pattern -->
      <div v-if="event.isRecurring && event.recurringPattern" class="recurring-info">
        <svg class="recurring-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path d="M23 4v6h-6"></path>
          <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"></path>
        </svg>
        <span class="recurring-text">
          Repeats {{ event.recurringPattern.frequency }}
          {{ event.recurringPattern.interval > 1 ? `every ${event.recurringPattern.interval} ${event.recurringPattern.frequency}s` : '' }}
        </span>
      </div>
    </main>

    <!-- RSVP Actions -->
    <footer v-if="interactive" class="event-actions">
      <div class="rsvp-buttons">
        <button 
          @click="handleRSVP('going')"
          :class="['rsvp-btn', 'going-btn', { 'active': event.rsvpStatus === 'going' }]"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <polyline points="20 6 9 17 4 12"></polyline>
          </svg>
          Going
        </button>
        
        <button 
          @click="handleRSVP('maybe')"
          :class="['rsvp-btn', 'maybe-btn', { 'active': event.rsvpStatus === 'maybe' }]"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <circle cx="12" cy="12" r="10"></circle>
            <path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"></path>
            <path d="M12 17h.01"></path>
          </svg>
          Maybe
        </button>
        
        <button 
          @click="handleRSVP('not_going')"
          :class="['rsvp-btn', 'not-going-btn', { 'active': event.rsvpStatus === 'not_going' }]"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <line x1="18" y1="6" x2="6" y2="18"></line>
            <line x1="6" y1="6" x2="18" y2="18"></line>
          </svg>
          Can't go
        </button>
      </div>

      <div class="secondary-actions">
        <button @click="handleCalendarAdd" class="action-btn calendar-btn">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <rect x="3" y="4" width="18" height="18" rx="2" ry="2"></rect>
            <line x1="16" y1="2" x2="16" y2="6"></line>
            <line x1="8" y1="2" x2="8" y2="6"></line>
            <line x1="3" y1="10" x2="21" y2="10"></line>
            <line x1="12" y1="14" x2="12" y2="18"></line>
            <line x1="10" y1="16" x2="14" y2="16"></line>
          </svg>
          Add to Calendar
        </button>
      </div>
    </footer>
  </div>
</template>

<script setup lang="ts">
import type { Event, LayoutMode } from '../../types/content'

interface Props {
  event: Event
  layout: LayoutMode
  interactive: boolean
}

const props = defineProps<Props>()

const emit = defineEmits<{
  rsvp: [status: 'going' | 'maybe' | 'not_going']
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

const formatEventDate = (timestamp: number) => {
  const date = new Date(timestamp)
  const now = new Date()
  const isToday = date.toDateString() === now.toDateString()
  const isTomorrow = date.toDateString() === new Date(now.getTime() + 86400000).toDateString()
  
  if (isToday) {
    return `Today at ${date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`
  } else if (isTomorrow) {
    return `Tomorrow at ${date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`
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

const getRSVPStatusText = (status: string) => {
  const statusMap = {
    going: '✓ Going',
    maybe: '? Maybe',
    not_going: '✗ Not Going',
    none: 'RSVP'
  }
  return statusMap[status as keyof typeof statusMap] || 'RSVP'
}

const handleRSVP = (status: 'going' | 'maybe' | 'not_going') => {
  emit('rsvp', status)
}

const handleCalendarAdd = () => {
  // Generate calendar URL (Google Calendar format)
  const startDate = new Date(props.event.startTime).toISOString().replace(/[-:]/g, '').split('.')[0] + 'Z'
  const endDate = new Date(props.event.endTime).toISOString().replace(/[-:]/g, '').split('.')[0] + 'Z'
  
  const calendarUrl = `https://calendar.google.com/calendar/render?action=TEMPLATE&text=${encodeURIComponent(props.event.title)}&dates=${startDate}/${endDate}&details=${encodeURIComponent(props.event.description || '')}&location=${encodeURIComponent(props.event.location?.address || props.event.location?.virtualUrl || '')}`
  
  window.open(calendarUrl, '_blank')
}
</script>

<style scoped>
.event-content {
  padding: 1rem;
  color: white;
}

.event-header {
  margin-bottom: 1rem;
}

.event-meta {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
}

.organizer-info {
  display: flex;
  align-items: center;
  gap: 0.75rem;
}

.organizer-avatar {
  width: 32px;
  height: 32px;
  border-radius: 50%;
  object-fit: cover;
}

.organizer-avatar-placeholder {
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

.organizer-details {
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
}

.organizer-name {
  font-weight: 600;
  font-size: 0.9rem;
  color: white;
}

.created-time {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
}

.rsvp-badge {
  padding: 0.25rem 0.75rem;
  border-radius: 12px;
  font-size: 0.7rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.025em;
}

.rsvp-going {
  background: rgba(34, 197, 94, 0.2);
  color: #22c55e;
}

.rsvp-maybe {
  background: rgba(245, 158, 11, 0.2);
  color: #f59e0b;
}

.rsvp-not_going {
  background: rgba(239, 68, 68, 0.2);
  color: #ef4444;
}

.rsvp-none {
  background: rgba(255, 255, 255, 0.1);
  color: rgba(255, 255, 255, 0.7);
}

.event-body {
  space-y: 1rem;
}

.event-title {
  font-size: 1.25rem;
  font-weight: 700;
  color: white;
  margin-bottom: 0.75rem;
  line-height: 1.3;
}

.event-description {
  color: rgba(255, 255, 255, 0.8);
  line-height: 1.5;
  margin-bottom: 1rem;
}

.event-datetime,
.event-location {
  display: flex;
  align-items: flex-start;
  gap: 0.75rem;
  margin-bottom: 0.75rem;
}

.datetime-icon,
.location-icon {
  width: 18px;
  height: 18px;
  color: var(--accent-primary);
  margin-top: 0.125rem;
  flex-shrink: 0;
}

.datetime-item {
  display: flex;
  align-items: flex-start;
  gap: 0.75rem;
}

.datetime-details {
  flex: 1;
}

.start-time {
  font-weight: 600;
  color: white;
  font-size: 0.9rem;
}

.end-time {
  color: rgba(255, 255, 255, 0.7);
  font-size: 0.8rem;
  margin-top: 0.125rem;
}

.location-details {
  flex: 1;
}

.location-type {
  font-weight: 500;
  color: white;
  font-size: 0.9rem;
}

.location-address {
  color: rgba(255, 255, 255, 0.7);
  font-size: 0.8rem;
  margin-top: 0.125rem;
  word-break: break-word;
}

.event-attendees {
  margin-bottom: 0.75rem;
}

.attendee-count {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-bottom: 0.5rem;
}

.attendee-icon {
  width: 16px;
  height: 16px;
  color: var(--accent-primary);
}

.count-text {
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

.recurring-info {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-bottom: 0.75rem;
}

.recurring-icon {
  width: 16px;
  height: 16px;
  color: var(--accent-primary);
}

.recurring-text {
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.7);
}

.event-actions {
  padding-top: 1rem;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}

.rsvp-buttons {
  display: flex;
  gap: 0.5rem;
  margin-bottom: 0.75rem;
  flex-wrap: wrap;
}

.rsvp-btn {
  display: flex;
  align-items: center;
  gap: 0.375rem;
  padding: 0.5rem 0.75rem;
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.05);
  color: rgba(255, 255, 255, 0.8);
  cursor: pointer;
  font-size: 0.8rem;
  font-weight: 500;
  transition: all 0.2s ease;
}

.rsvp-btn:hover {
  background: rgba(255, 255, 255, 0.1);
  border-color: rgba(255, 255, 255, 0.3);
}

.rsvp-btn svg {
  width: 14px;
  height: 14px;
}

.going-btn.active {
  background: rgba(34, 197, 94, 0.2);
  border-color: #22c55e;
  color: #22c55e;
}

.maybe-btn.active {
  background: rgba(245, 158, 11, 0.2);
  border-color: #f59e0b;
  color: #f59e0b;
}

.not-going-btn.active {
  background: rgba(239, 68, 68, 0.2);
  border-color: #ef4444;
  color: #ef4444;
}

.secondary-actions {
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

@media (max-width: 480px) {
  .rsvp-buttons {
    flex-direction: column;
  }
  
  .rsvp-btn {
    justify-content: center;
  }
  
  .event-meta {
    flex-direction: column;
    gap: 0.75rem;
    align-items: flex-start;
  }
}
</style>
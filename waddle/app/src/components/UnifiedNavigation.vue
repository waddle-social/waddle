<template>
  <nav class="unified-navigation">
    <!-- Main Navigation Header -->
    <header class="nav-header">
      <div class="nav-container">
        <!-- Brand/Logo -->
        <div class="brand-section">
          <div class="brand-logo">
            <h1 class="brand-text">🏠 Waddle</h1>
          </div>
          
          <!-- Global Search -->
          <div class="search-section">
            <div class="search-container">
              <div class="search-input-wrapper">
                <svg class="search-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                  <circle cx="11" cy="11" r="8"></circle>
                  <path d="m21 21-4.35-4.35"></path>
                </svg>
                <input
                  v-model="searchQuery"
                  @input="handleSearch"
                  type="text"
                  placeholder="Search everything..."
                  class="search-input"
                />
                <button
                  v-if="searchQuery"
                  @click="clearSearch"
                  class="search-clear"
                >
                  ×
                </button>
              </div>
              
              <!-- Search Suggestions -->
              <div v-if="showSearchSuggestions && searchSuggestions.length > 0" class="search-suggestions">
                <div
                  v-for="suggestion in searchSuggestions"
                  :key="suggestion.id"
                  @click="selectSearchSuggestion(suggestion)"
                  class="search-suggestion"
                >
                  <span class="suggestion-icon">{{ suggestion.icon }}</span>
                  <span class="suggestion-text">{{ suggestion.text }}</span>
                  <span class="suggestion-type">{{ suggestion.type }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- Global Actions -->
        <div class="actions-section">
          <!-- Quick Filters -->
          <div class="quick-filters">
            <select
              v-model="activeTimeFilter"
              @change="handleTimeFilterChange"
              class="time-filter"
            >
              <option value="hour">Last Hour</option>
              <option value="day">Today</option>
              <option value="week">This Week</option>
              <option value="month">This Month</option>
              <option value="all">All Time</option>
            </select>
          </div>

          <!-- Notifications -->
          <button
            @click="toggleNotifications"
            class="notification-button"
            :class="{ 'has-notifications': unreadNotifications > 0 }"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9"></path>
              <path d="M13.73 21a2 2 0 0 1-3.46 0"></path>
            </svg>
            <span v-if="unreadNotifications > 0" class="notification-badge">
              {{ unreadNotifications > 99 ? '99+' : unreadNotifications }}
            </span>
          </button>

          <!-- User Menu -->
          <div class="user-menu">
            <button
              @click="toggleUserMenu"
              class="user-avatar"
            >
              <img
                v-if="user?.avatar"
                :src="user.avatar"
                :alt="user.displayName"
                class="avatar-image"
              />
              <div v-else class="avatar-placeholder">
                {{ user?.displayName?.charAt(0) || 'U' }}
              </div>
            </button>

            <!-- User Dropdown -->
            <div v-if="showUserMenu" class="user-dropdown">
              <div class="user-info">
                <div class="user-name">{{ user?.displayName }}</div>
                <div class="user-username">@{{ user?.username }}</div>
              </div>
              <hr class="dropdown-divider" />
              <button @click="navigateTo('/profile')" class="dropdown-item">
                Profile
              </button>
              <button @click="navigateTo('/settings')" class="dropdown-item">
                Settings
              </button>
              <hr class="dropdown-divider" />
              <button @click="handleLogout" class="dropdown-item logout">
                Logout
              </button>
            </div>
          </div>

          <!-- Settings -->
          <button @click="navigateTo('/settings')" class="settings-button">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <circle cx="12" cy="12" r="3"></circle>
              <path d="m12 1 2.09 12L12 23l-2.09-10L12 1z"></path>
              <path d="M16.5 8.5 12 12l-4.5-3.5L12 5l4.5 3.5z"></path>
            </svg>
          </button>
        </div>
      </div>
    </header>

    <!-- Main Tabs Navigation -->
    <div class="tabs-navigation">
      <div class="nav-container">
        <!-- Primary Tabs -->
        <div class="primary-tabs">
          <button
            v-for="tab in primaryTabs"
            :key="tab.id"
            @click="switchToView(tab.id)"
            :class="[
              'nav-tab',
              { 'active': currentView === tab.id },
              { 'has-updates': tab.hasUpdates }
            ]"
          >
            <span class="tab-icon">{{ tab.icon }}</span>
            <span class="tab-label">{{ tab.label }}</span>
            <span v-if="tab.hasUpdates" class="tab-badge">
              {{ tab.updateCount || '•' }}
            </span>
          </button>
        </div>

        <!-- Custom Views -->
        <div v-if="customViews.length > 0" class="custom-views">
          <div class="custom-views-separator">|</div>
          <div
            v-for="view in customViews"
            :key="view.id"
            :class="[
              'custom-view-wrapper',
              { 'active': activeCustomView?.id === view.id }
            ]"
          >
            <button
              @click="switchToCustomView(view)"
              class="nav-tab custom-view"
            >
              <span class="tab-icon">{{ view.icon || '📋' }}</span>
              <span class="tab-label">{{ view.name }}</span>
            </button>
            <button
              @click.stop="editCustomView(view)"
              class="edit-view-button"
            >
              <svg viewBox="0 0 16 16" fill="currentColor">
                <path d="M12.146.854a.5.5 0 0 1 .708 0l2.5 2.5a.5.5 0 0 1 0 .708l-10 10a.5.5 0 0 1-.168.11L1.5 15.5a.5.5 0 0 1-.62-.62l1.328-3.686a.5.5 0 0 1 .11-.168l10-10z"/>
              </svg>
            </button>
          </div>
          
          <!-- Add Custom View Button -->
          <button
            @click="createNewView"
            class="nav-tab add-view"
            title="Create custom view"
          >
            <span class="tab-icon">+</span>
            <span class="tab-label">New View</span>
          </button>
        </div>

        <!-- View Controls -->
        <div class="view-controls">
          <!-- Layout Toggle -->
          <div class="layout-controls">
            <button
              v-for="layout in layoutOptions"
              :key="layout.id"
              @click="changeLayout(layout.id)"
              :class="[
                'layout-button',
                { 'active': currentLayout === layout.id }
              ]"
              :title="layout.label"
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
                <path :d="layout.iconPath"></path>
              </svg>
            </button>
          </div>

          <!-- Real-time Toggle -->
          <button
            @click="toggleRealTime"
            :class="[
              'realtime-toggle',
              { 'active': realTimeEnabled }
            ]"
            title="Toggle real-time updates"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <circle cx="12" cy="12" r="3"></circle>
              <path d="M12 1l2.09 12L12 23l-2.09-10L12 1z" :class="{ 'pulse': realTimeEnabled }"></path>
            </svg>
            <span class="realtime-label">Live</span>
          </button>
        </div>
      </div>
    </div>

    <!-- Notifications Panel -->
    <Transition name="slide-down">
      <div v-if="showNotifications" class="notifications-panel">
        <div class="panel-header">
          <h3>Notifications</h3>
          <button @click="markAllAsRead" class="mark-all-read">
            Mark all read
          </button>
        </div>
        <div class="notifications-list">
          <div
            v-for="notification in notifications"
            :key="notification.id"
            :class="[
              'notification-item',
              { 'unread': !notification.isRead }
            ]"
            @click="handleNotificationClick(notification)"
          >
            <div class="notification-content">
              <div class="notification-title">{{ notification.title }}</div>
              <div class="notification-message">{{ notification.message }}</div>
              <div class="notification-time">{{ formatTime(notification.timestamp) }}</div>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </nav>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useActor } from '@xstate/vue'
import type { ViewType, CustomView, User, Notification } from '../types/content'

interface SearchSuggestion {
  id: string
  text: string
  type: string
  icon: string
}

interface NavigationTab {
  id: ViewType
  label: string
  icon: string
  hasUpdates: boolean
  updateCount?: number
}

interface LayoutOption {
  id: string
  label: string
  iconPath: string
}

const props = defineProps<{
  user: User | null
  currentView: ViewType
  activeCustomView?: CustomView
  customViews: CustomView[]
  realTimeEnabled: boolean
  currentLayout: string
}>()

const emit = defineEmits<{
  switchView: [viewType: ViewType]
  switchToCustomView: [view: CustomView]
  search: [query: string]
  toggleRealTime: []
  changeLayout: [layout: string]
  createCustomView: []
  editCustomView: [view: CustomView]
  logout: []
}>()

// Reactive state
const searchQuery = ref('')
const showSearchSuggestions = ref(false)
const searchSuggestions = ref<SearchSuggestion[]>([])
const activeTimeFilter = ref('day')
const showNotifications = ref(false)
const showUserMenu = ref(false)
const unreadNotifications = ref(3)
const notifications = ref<Notification[]>([])

// Primary navigation tabs
const primaryTabs = computed<NavigationTab[]>(() => [
  {
    id: 'feed',
    label: 'Feed',
    icon: '🔥',
    hasUpdates: true,
    updateCount: 12
  },
  {
    id: 'chat',
    label: 'Chat',
    icon: '💬',
    hasUpdates: true,
    updateCount: 5
  },
  {
    id: 'events',
    label: 'Events',
    icon: '📅',
    hasUpdates: false
  },
  {
    id: 'people',
    label: 'People',
    icon: '👥',
    hasUpdates: false
  },
  {
    id: 'links',
    label: 'Links',
    icon: '🔗',
    hasUpdates: true,
    updateCount: 2
  },
  {
    id: 'hangouts',
    label: 'Hangouts',
    icon: '🎧',
    hasUpdates: true
  },
  {
    id: 'messages',
    label: 'Messages',
    icon: '💌',
    hasUpdates: true,
    updateCount: 8
  }
])

// Layout options
const layoutOptions = computed<LayoutOption[]>(() => [
  {
    id: 'feed',
    label: 'Feed Layout',
    iconPath: 'M8 2v20M2 8h20M2 16h20'
  },
  {
    id: 'grid',
    label: 'Grid Layout',
    iconPath: 'M3 3h7v7H3zM14 3h7v7h-7zM14 14h7v7h-7zM3 14h7v7H3z'
  },
  {
    id: 'timeline',
    label: 'Timeline Layout',
    iconPath: 'M12 2v20M8 6l4-4 4 4M8 18l4 4 4-4'
  }
])

// Event handlers
const handleSearch = () => {
  if (searchQuery.value.length > 0) {
    showSearchSuggestions.value = true
    // Mock search suggestions - replace with real search API
    searchSuggestions.value = [
      { id: '1', text: `"${searchQuery.value}" in messages`, type: 'search', icon: '💬' },
      { id: '2', text: `Events about "${searchQuery.value}"`, type: 'events', icon: '📅' },
      { id: '3', text: `People named "${searchQuery.value}"`, type: 'people', icon: '👥' },
    ]
  } else {
    showSearchSuggestions.value = false
    searchSuggestions.value = []
  }
  
  emit('search', searchQuery.value)
}

const clearSearch = () => {
  searchQuery.value = ''
  showSearchSuggestions.value = false
  searchSuggestions.value = []
  emit('search', '')
}

const selectSearchSuggestion = (suggestion: SearchSuggestion) => {
  searchQuery.value = suggestion.text
  showSearchSuggestions.value = false
  emit('search', suggestion.text)
}

const handleTimeFilterChange = () => {
  // Emit filter change event
}

const switchToView = (viewType: ViewType) => {
  emit('switchView', viewType)
}

const switchToCustomView = (view: CustomView) => {
  emit('switchToCustomView', view)
}

const toggleNotifications = () => {
  showNotifications.value = !showNotifications.value
  showUserMenu.value = false
}

const toggleUserMenu = () => {
  showUserMenu.value = !showUserMenu.value
  showNotifications.value = false
}

const toggleRealTime = () => {
  emit('toggleRealTime')
}

const changeLayout = (layout: string) => {
  emit('changeLayout', layout)
}

const createNewView = () => {
  emit('createCustomView')
}

const editCustomView = (view: CustomView) => {
  emit('editCustomView', view)
}

const navigateTo = (path: string) => {
  // Handle navigation
  console.log('Navigate to:', path)
}

const handleLogout = () => {
  emit('logout')
}

const markAllAsRead = () => {
  unreadNotifications.value = 0
  notifications.value.forEach(n => n.isRead = true)
}

const handleNotificationClick = (notification: Notification) => {
  notification.isRead = true
  if (unreadNotifications.value > 0) {
    unreadNotifications.value--
  }
  // Navigate to notification content
  if (notification.actionUrl) {
    navigateTo(notification.actionUrl)
  }
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

// Click outside handlers
const handleClickOutside = (event: Event) => {
  const target = event.target as HTMLElement
  
  // Close search suggestions if clicking outside
  if (!target.closest('.search-container')) {
    showSearchSuggestions.value = false
  }
  
  // Close notifications if clicking outside
  if (!target.closest('.notification-button') && !target.closest('.notifications-panel')) {
    showNotifications.value = false
  }
  
  // Close user menu if clicking outside
  if (!target.closest('.user-menu')) {
    showUserMenu.value = false
  }
}

// Keyboard shortcuts
const handleKeydown = (event: KeyboardEvent) => {
  // Ctrl/Cmd + K = Focus search
  if ((event.ctrlKey || event.metaKey) && event.key === 'k') {
    event.preventDefault()
    const searchInput = document.querySelector('.search-input') as HTMLInputElement
    searchInput?.focus()
  }
  
  // Escape = Close panels
  if (event.key === 'Escape') {
    showSearchSuggestions.value = false
    showNotifications.value = false
    showUserMenu.value = false
  }
  
  // Number keys = Switch views
  const numKey = parseInt(event.key)
  if (numKey >= 1 && numKey <= primaryTabs.value.length && !event.ctrlKey && !event.metaKey) {
    const targetTab = primaryTabs.value[numKey - 1]
    if (targetTab) {
      switchToView(targetTab.id)
    }
  }
}

onMounted(() => {
  document.addEventListener('click', handleClickOutside)
  document.addEventListener('keydown', handleKeydown)
  
  // Mock notifications - replace with real data
  notifications.value = [
    {
      id: '1',
      userId: 'current-user',
      type: 'mention',
      title: 'New mention',
      message: 'John mentioned you in chat',
      timestamp: Date.now() - 300000,
      isRead: false,
      fromUserId: 'john',
      fromUsername: 'john',
    },
    {
      id: '2',
      userId: 'current-user',
      type: 'event',
      title: 'Event reminder',
      message: 'Tech meetup starts in 1 hour',
      timestamp: Date.now() - 600000,
      isRead: false,
    },
    {
      id: '3',
      userId: 'current-user',
      type: 'message',
      title: 'New message',
      message: 'Sarah sent you a direct message',
      timestamp: Date.now() - 900000,
      isRead: true,
      fromUserId: 'sarah',
      fromUsername: 'sarah',
    },
  ]
})

onUnmounted(() => {
  document.removeEventListener('click', handleClickOutside)
  document.removeEventListener('keydown', handleKeydown)
})
</script>

<style scoped>
/* Navigation Container */
.unified-navigation {
  position: sticky;
  top: 0;
  z-index: 100;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

/* Header Styles */
.nav-header {
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
}

.nav-container {
  max-width: 1400px;
  margin: 0 auto;
  padding: 0 1.5rem;
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 60px;
}

.brand-section {
  display: flex;
  align-items: center;
  gap: 2rem;
  flex: 1;
}

.brand-text {
  font-size: 1.25rem;
  font-weight: 700;
  color: white;
  margin: 0;
}

/* Search Styles */
.search-section {
  flex: 1;
  max-width: 400px;
  position: relative;
}

.search-container {
  position: relative;
}

.search-input-wrapper {
  position: relative;
  display: flex;
  align-items: center;
}

.search-input {
  width: 100%;
  height: 40px;
  background: rgba(255, 255, 255, 0.1);
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 20px;
  padding: 0 40px 0 40px;
  color: white;
  font-size: 0.9rem;
  transition: all 0.2s ease;
}

.search-input:focus {
  outline: none;
  background: rgba(255, 255, 255, 0.15);
  border-color: var(--accent-primary);
  box-shadow: 0 0 0 3px rgba(var(--accent-primary-rgb), 0.1);
}

.search-input::placeholder {
  color: rgba(255, 255, 255, 0.5);
}

.search-icon {
  position: absolute;
  left: 12px;
  width: 16px;
  height: 16px;
  color: rgba(255, 255, 255, 0.5);
  pointer-events: none;
}

.search-clear {
  position: absolute;
  right: 12px;
  width: 20px;
  height: 20px;
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.5);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 1.2rem;
  transition: color 0.2s ease;
}

.search-clear:hover {
  color: rgba(255, 255, 255, 0.8);
}

/* Search Suggestions */
.search-suggestions {
  position: absolute;
  top: 100%;
  left: 0;
  right: 0;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  margin-top: 4px;
  overflow: hidden;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
}

.search-suggestion {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding: 0.75rem 1rem;
  cursor: pointer;
  transition: background-color 0.15s ease;
  border: none;
  background: none;
  width: 100%;
  text-align: left;
  color: white;
}

.search-suggestion:hover {
  background: rgba(255, 255, 255, 0.1);
}

.suggestion-icon {
  font-size: 1rem;
  opacity: 0.8;
}

.suggestion-text {
  flex: 1;
  font-size: 0.9rem;
}

.suggestion-type {
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

/* Actions Section */
.actions-section {
  display: flex;
  align-items: center;
  gap: 1rem;
}

.time-filter {
  background: rgba(255, 255, 255, 0.1);
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 8px;
  padding: 0.5rem 0.75rem;
  color: white;
  font-size: 0.85rem;
  cursor: pointer;
  transition: all 0.2s ease;
}

.time-filter:hover {
  background: rgba(255, 255, 255, 0.15);
}

.time-filter:focus {
  outline: none;
  border-color: var(--accent-primary);
}

/* Notification Button */
.notification-button {
  position: relative;
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.7);
  cursor: pointer;
  padding: 0.5rem;
  border-radius: 8px;
  transition: all 0.2s ease;
}

.notification-button:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.notification-button.has-notifications {
  color: var(--accent-primary);
}

.notification-button svg {
  width: 20px;
  height: 20px;
}

.notification-badge {
  position: absolute;
  top: 2px;
  right: 2px;
  background: var(--accent-primary);
  color: white;
  font-size: 0.7rem;
  font-weight: 600;
  padding: 2px 6px;
  border-radius: 10px;
  min-width: 18px;
  text-align: center;
  line-height: 1;
}

/* User Menu */
.user-menu {
  position: relative;
}

.user-avatar {
  background: none;
  border: none;
  cursor: pointer;
  padding: 0;
  border-radius: 50%;
  overflow: hidden;
  transition: all 0.2s ease;
}

.user-avatar:hover {
  transform: scale(1.05);
  box-shadow: 0 0 0 2px var(--accent-primary);
}

.avatar-image {
  width: 32px;
  height: 32px;
  border-radius: 50%;
  object-fit: cover;
}

.avatar-placeholder {
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

.user-dropdown {
  position: absolute;
  top: 100%;
  right: 0;
  width: 200px;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  margin-top: 8px;
  overflow: hidden;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
}

.user-info {
  padding: 1rem;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.user-name {
  color: white;
  font-weight: 600;
  font-size: 0.9rem;
}

.user-username {
  color: rgba(255, 255, 255, 0.6);
  font-size: 0.8rem;
  margin-top: 0.25rem;
}

.dropdown-divider {
  border: none;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  margin: 0;
}

.dropdown-item {
  display: block;
  width: 100%;
  padding: 0.75rem 1rem;
  background: none;
  border: none;
  color: white;
  text-align: left;
  font-size: 0.9rem;
  cursor: pointer;
  transition: background-color 0.15s ease;
}

.dropdown-item:hover {
  background: rgba(255, 255, 255, 0.1);
}

.dropdown-item.logout {
  color: #ef4444;
}

.dropdown-item.logout:hover {
  background: rgba(239, 68, 68, 0.1);
}

/* Settings Button */
.settings-button {
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.7);
  cursor: pointer;
  padding: 0.5rem;
  border-radius: 8px;
  transition: all 0.2s ease;
}

.settings-button:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.settings-button svg {
  width: 18px;
  height: 18px;
}

/* Tabs Navigation */
.tabs-navigation {
  overflow-x: auto;
  scrollbar-width: none;
  -ms-overflow-style: none;
}

.tabs-navigation::-webkit-scrollbar {
  display: none;
}

.tabs-navigation .nav-container {
  height: 50px;
  gap: 2rem;
  min-width: max-content;
}

.primary-tabs {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.nav-tab {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.5rem 0.75rem;
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.7);
  cursor: pointer;
  border-radius: 8px;
  font-size: 0.9rem;
  font-weight: 500;
  transition: all 0.2s ease;
  position: relative;
  white-space: nowrap;
}

.nav-tab:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.nav-tab.active {
  color: var(--accent-primary);
  background: rgba(var(--accent-primary-rgb), 0.15);
}

.nav-tab.has-updates::after {
  content: '';
  position: absolute;
  top: 4px;
  right: 4px;
  width: 6px;
  height: 6px;
  background: var(--accent-primary);
  border-radius: 50%;
}

.tab-icon {
  font-size: 1rem;
  line-height: 1;
}

.tab-label {
  font-weight: 500;
}

.tab-badge {
  background: var(--accent-primary);
  color: white;
  font-size: 0.7rem;
  font-weight: 600;
  padding: 2px 6px;
  border-radius: 8px;
  min-width: 16px;
  text-align: center;
  line-height: 1;
}

/* Custom Views */
.custom-views {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.custom-views-separator {
  color: rgba(255, 255, 255, 0.3);
  margin: 0 0.5rem;
}

.custom-view-wrapper {
  position: relative;
  display: flex;
  align-items: center;
}

.custom-view-wrapper .nav-tab {
  margin: 0;
}

.edit-view-button {
  position: absolute;
  top: -4px;
  right: -4px;
  background: rgba(0, 0, 0, 0.8);
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 4px;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  padding: 2px;
  opacity: 0;
  transition: all 0.2s ease;
}

.custom-view-wrapper:hover .edit-view-button {
  opacity: 1;
}

.edit-view-button:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.edit-view-button svg {
  width: 12px;
  height: 12px;
}

.add-view {
  border: 1px dashed rgba(255, 255, 255, 0.3);
  background: rgba(255, 255, 255, 0.05);
}

.add-view:hover {
  border-color: var(--accent-primary);
  background: rgba(var(--accent-primary-rgb), 0.1);
  color: var(--accent-primary);
}

/* View Controls */
.view-controls {
  display: flex;
  align-items: center;
  gap: 1rem;
  margin-left: auto;
}

.layout-controls {
  display: flex;
  align-items: center;
  gap: 0.25rem;
}

.layout-button {
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  padding: 0.5rem;
  border-radius: 6px;
  transition: all 0.2s ease;
}

.layout-button:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.layout-button.active {
  color: var(--accent-primary);
  background: rgba(var(--accent-primary-rgb), 0.15);
}

.layout-button svg {
  width: 16px;
  height: 16px;
}

/* Real-time Toggle */
.realtime-toggle {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  background: none;
  border: 1px solid rgba(255, 255, 255, 0.2);
  color: rgba(255, 255, 255, 0.7);
  cursor: pointer;
  padding: 0.5rem 0.75rem;
  border-radius: 8px;
  font-size: 0.8rem;
  font-weight: 500;
  transition: all 0.2s ease;
}

.realtime-toggle:hover {
  color: white;
  border-color: rgba(255, 255, 255, 0.4);
}

.realtime-toggle.active {
  color: var(--accent-primary);
  border-color: var(--accent-primary);
  background: rgba(var(--accent-primary-rgb), 0.1);
}

.realtime-toggle svg {
  width: 14px;
  height: 14px;
}

.realtime-toggle .pulse {
  animation: pulse 2s infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.5; }
}

/* Notifications Panel */
.notifications-panel {
  position: absolute;
  top: 100%;
  right: 1.5rem;
  width: 320px;
  max-height: 400px;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  margin-top: 8px;
  overflow: hidden;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
}

.panel-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 1rem;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.panel-header h3 {
  color: white;
  font-size: 1rem;
  font-weight: 600;
  margin: 0;
}

.mark-all-read {
  background: none;
  border: none;
  color: var(--accent-primary);
  cursor: pointer;
  font-size: 0.8rem;
  font-weight: 500;
  transition: opacity 0.2s ease;
}

.mark-all-read:hover {
  opacity: 0.8;
}

.notifications-list {
  max-height: 300px;
  overflow-y: auto;
}

.notification-item {
  padding: 0.75rem 1rem;
  cursor: pointer;
  transition: background-color 0.15s ease;
  border-left: 3px solid transparent;
}

.notification-item:hover {
  background: rgba(255, 255, 255, 0.05);
}

.notification-item.unread {
  border-left-color: var(--accent-primary);
  background: rgba(var(--accent-primary-rgb), 0.05);
}

.notification-content {
  color: white;
}

.notification-title {
  font-size: 0.9rem;
  font-weight: 600;
  margin-bottom: 0.25rem;
}

.notification-message {
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.8);
  margin-bottom: 0.5rem;
}

.notification-time {
  font-size: 0.7rem;
  color: rgba(255, 255, 255, 0.5);
}

/* Transitions */
.slide-down-enter-active,
.slide-down-leave-active {
  transition: all 0.3s cubic-bezier(0.4, 0, 0.2, 1);
}

.slide-down-enter-from,
.slide-down-leave-to {
  opacity: 0;
  transform: translateY(-10px);
}

/* Responsive Design */
@media (max-width: 768px) {
  .nav-container {
    padding: 0 1rem;
  }
  
  .brand-section {
    gap: 1rem;
  }
  
  .search-section {
    max-width: 200px;
  }
  
  .actions-section {
    gap: 0.5rem;
  }
  
  .tab-label {
    display: none;
  }
  
  .custom-views {
    display: none;
  }
  
  .view-controls {
    gap: 0.5rem;
  }
  
  .notifications-panel {
    right: 1rem;
    width: 280px;
  }
}

@media (max-width: 480px) {
  .nav-container {
    height: 50px;
  }
  
  .search-section {
    display: none;
  }
  
  .time-filter {
    display: none;
  }
  
  .realtime-label {
    display: none;
  }
}
</style>
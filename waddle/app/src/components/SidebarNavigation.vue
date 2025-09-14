<template>
  <aside 
    :class="[
      'sidebar-navigation',
      { 'collapsed': isCollapsed },
      { 'mobile-open': isMobileMenuOpen }
    ]"
  >
    <!-- Sidebar Header -->
    <header class="sidebar-header">
      <div class="brand-section">
        <div class="brand-logo">
          <span class="brand-icon">🏠</span>
          <Transition name="fade">
            <span v-show="!isCollapsed" class="brand-text">Waddle</span>
          </Transition>
        </div>
        <button
          @click="toggleSidebar"
          class="sidebar-toggle"
          :title="isCollapsed ? 'Expand sidebar' : 'Collapse sidebar'"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path :d="isCollapsed ? 'M9 18l6-6-6-6' : 'M15 18l-6-6 6-6'"></path>
          </svg>
        </button>
      </div>

      <!-- User Profile -->
      <div class="user-profile" v-if="!isCollapsed">
        <div class="user-avatar-section">
          <img 
            v-if="user?.avatar" 
            :src="user.avatar" 
            :alt="user.displayName"
            class="user-avatar"
          />
          <div v-else class="user-avatar-placeholder">
            {{ user?.displayName?.charAt(0) || 'U' }}
          </div>
          <div class="online-indicator"></div>
        </div>
        <div class="user-details">
          <div class="user-name">{{ user?.displayName }}</div>
          <div class="user-status">Online</div>
        </div>
        <button @click="toggleUserMenu" class="user-menu-toggle">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <circle cx="12" cy="12" r="1"></circle>
            <circle cx="19" cy="12" r="1"></circle>
            <circle cx="5" cy="12" r="1"></circle>
          </svg>
        </button>
      </div>
    </header>

    <!-- Global Search -->
    <div class="search-section" v-if="!isCollapsed">
      <div class="search-input-wrapper">
        <svg class="search-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <circle cx="11" cy="11" r="8"></circle>
          <path d="m21 21-4.35-4.35"></path>
        </svg>
        <input
          v-model="searchQuery"
          @input="handleSearch"
          type="text"
          placeholder="Search Waddle..."
          class="search-input"
        />
        <kbd class="search-shortcut">⌘K</kbd>
      </div>
    </div>

    <!-- Main Navigation -->
    <nav class="main-navigation">
      <div class="nav-section">
        <div class="nav-section-title" v-if="!isCollapsed">
          <span>Community</span>
        </div>
        
        <div class="nav-items">
          <button
            v-for="item in mainNavItems"
            :key="item.id"
            @click="handleNavigation(item.id)"
            :class="[
              'nav-item',
              { 'active': currentView === item.id },
              { 'has-updates': item.hasUpdates }
            ]"
            :title="isCollapsed ? item.label : ''"
          >
            <span class="nav-icon">{{ item.icon }}</span>
            <Transition name="fade">
              <span v-show="!isCollapsed" class="nav-label">{{ item.label }}</span>
            </Transition>
            <span 
              v-if="item.hasUpdates && !isCollapsed" 
              class="nav-badge"
            >
              {{ item.updateCount || '•' }}
            </span>
            <div v-if="item.hasUpdates && isCollapsed" class="nav-dot"></div>
          </button>
        </div>
      </div>

      <!-- Direct Messages -->
      <div class="nav-section">
        <div class="nav-section-title" v-if="!isCollapsed">
          <span>Direct Messages</span>
          <button class="section-action" title="Start new conversation">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <line x1="12" y1="5" x2="12" y2="19"></line>
              <line x1="5" y1="12" x2="19" y2="12"></line>
            </svg>
          </button>
        </div>
        
        <div class="nav-items">
          <button
            v-for="dm in directMessages"
            :key="dm.id"
            @click="handleDirectMessage(dm.id)"
            :class="[
              'nav-item dm-item',
              { 'active': currentDM === dm.id },
              { 'has-unread': dm.hasUnread }
            ]"
            :title="isCollapsed ? dm.name : ''"
          >
            <div class="dm-avatar">
              <img v-if="dm.avatar" :src="dm.avatar" :alt="dm.name" />
              <div v-else class="dm-avatar-placeholder">
                {{ dm.name.charAt(0) }}
              </div>
              <div v-if="dm.isOnline" class="dm-online-indicator"></div>
            </div>
            <Transition name="fade">
              <div v-show="!isCollapsed" class="dm-details">
                <span class="dm-name">{{ dm.name }}</span>
                <span class="dm-status">{{ dm.lastMessage }}</span>
              </div>
            </Transition>
            <span 
              v-if="dm.hasUnread && !isCollapsed" 
              class="nav-badge unread"
            >
              {{ dm.unreadCount }}
            </span>
            <div v-if="dm.hasUnread && isCollapsed" class="nav-dot unread"></div>
          </button>
        </div>
      </div>

      <!-- Settings & Actions -->
      <div class="nav-section nav-section-bottom">
        <div class="nav-items">
          <button
            @click="handleSettings"
            class="nav-item"
            :title="isCollapsed ? 'Settings' : ''"
          >
            <span class="nav-icon">⚙️</span>
            <Transition name="fade">
              <span v-show="!isCollapsed" class="nav-label">Settings</span>
            </Transition>
          </button>
          
          <button
            @click="toggleRealTime"
            :class="[
              'nav-item',
              { 'active': realTimeEnabled }
            ]"
            :title="isCollapsed ? 'Toggle real-time' : ''"
          >
            <span class="nav-icon">{{ realTimeEnabled ? '🟢' : '⏸️' }}</span>
            <Transition name="fade">
              <span v-show="!isCollapsed" class="nav-label">
                {{ realTimeEnabled ? 'Live Updates' : 'Paused' }}
              </span>
            </Transition>
          </button>
        </div>
      </div>
    </nav>

    <!-- User Menu Dropdown -->
    <Transition name="slide-up">
      <div v-if="showUserMenu && !isCollapsed" class="user-menu-dropdown">
        <div class="user-menu-header">
          <div class="user-menu-avatar">
            <img v-if="user?.avatar" :src="user.avatar" :alt="user.displayName" />
            <div v-else class="avatar-placeholder">{{ user?.displayName?.charAt(0) }}</div>
          </div>
          <div class="user-menu-info">
            <div class="user-menu-name">{{ user?.displayName }}</div>
            <div class="user-menu-username">@{{ user?.username }}</div>
          </div>
        </div>
        
        <div class="user-menu-items">
          <button class="user-menu-item">
            <span class="menu-icon">👤</span>
            <span>Profile</span>
          </button>
          <button class="user-menu-item">
            <span class="menu-icon">🔔</span>
            <span>Notifications</span>
          </button>
          <button class="user-menu-item">
            <span class="menu-icon">🎨</span>
            <span>Appearance</span>
          </button>
          <hr class="menu-divider" />
          <button @click="handleLogout" class="user-menu-item logout">
            <span class="menu-icon">🚪</span>
            <span>Logout</span>
          </button>
        </div>
      </div>
    </Transition>
  </aside>

  <!-- Mobile Menu Backdrop -->
  <div 
    v-if="isMobileMenuOpen" 
    class="mobile-backdrop"
    @click="closeMobileMenu"
  ></div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import type { ViewType, User } from '../types/content'

interface NavigationItem {
  id: ViewType
  label: string
  icon: string
  hasUpdates: boolean
  updateCount?: number
}

interface DirectMessage {
  id: string
  name: string
  avatar?: string
  isOnline: boolean
  hasUnread: boolean
  unreadCount: number
  lastMessage: string
}

interface Props {
  user: User | null
  currentView: ViewType
  currentDM?: string
  collapsed?: boolean
}

const props = withDefaults(defineProps<Props>(), {
  collapsed: false
})

const emit = defineEmits<{
  switchView: [viewType: ViewType]
  toggleSidebar: []
  search: [query: string]
  logout: []
}>()

// Reactive state
const searchQuery = ref('')
const showUserMenu = ref(false)
const isMobileMenuOpen = ref(false)

// Computed properties
const isCollapsed = computed(() => props.collapsed)

// Mock data
const mainNavItems = computed<NavigationItem[]>(() => [
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
  }
])

const directMessages = computed<DirectMessage[]>(() => [
  {
    id: 'dm_1',
    name: 'Alice Johnson',
    isOnline: true,
    hasUnread: true,
    unreadCount: 3,
    lastMessage: 'Hey, did you see the new...'
  },
  {
    id: 'dm_2',
    name: 'Bob Smith',
    isOnline: false,
    hasUnread: false,
    unreadCount: 0,
    lastMessage: 'Thanks for the help!'
  },
  {
    id: 'dm_3',
    name: 'Team Alpha',
    isOnline: true,
    hasUnread: true,
    unreadCount: 7,
    lastMessage: 'Meeting at 3pm today'
  }
])

// Event handlers
const handleNavigation = (viewType: ViewType) => {
  emit('switchView', viewType)
}

const handleDirectMessage = (dmId: string) => {
  console.log('Open DM:', dmId)
}

const handleSearch = () => {
  emit('search', searchQuery.value)
}

const toggleSidebar = () => {
  emit('toggleSidebar')
}

const toggleUserMenu = () => {
  showUserMenu.value = !showUserMenu.value
}

const toggleRealTime = () => {
  emit('toggleRealTime')
}

const handleSettings = () => {
  console.log('Open settings')
}

const handleLogout = () => {
  emit('logout')
}

const closeMobileMenu = () => {
  isMobileMenuOpen.value = false
}

// Keyboard shortcuts
const handleKeydown = (event: KeyboardEvent) => {
  // Cmd/Ctrl + K = Focus search
  if ((event.ctrlKey || event.metaKey) && event.key === 'k') {
    event.preventDefault()
    const searchInput = document.querySelector('.search-input') as HTMLInputElement
    searchInput?.focus()
  }
  
  // Escape = Close menus
  if (event.key === 'Escape') {
    showUserMenu.value = false
  }
}

// Click outside handler
const handleClickOutside = (event: Event) => {
  const target = event.target as HTMLElement
  
  if (!target.closest('.user-profile') && !target.closest('.user-menu-dropdown')) {
    showUserMenu.value = false
  }
}

onMounted(() => {
  document.addEventListener('keydown', handleKeydown)
  document.addEventListener('click', handleClickOutside)
})

onUnmounted(() => {
  document.removeEventListener('keydown', handleKeydown)
  document.removeEventListener('click', handleClickOutside)
})
</script>

<style scoped>
/* Sidebar Container */
.sidebar-navigation {
  position: fixed;
  left: 0;
  top: 0;
  height: 100vh;
  width: 280px;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border-right: 1px solid rgba(255, 255, 255, 0.1);
  display: flex;
  flex-direction: column;
  transition: width 0.3s cubic-bezier(0.4, 0, 0.2, 1);
  z-index: 1000;
  overflow: hidden;
}

.sidebar-navigation.collapsed {
  width: 80px;
}

/* Header */
.sidebar-header {
  padding: 1rem;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.brand-section {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 1rem;
}

.brand-logo {
  display: flex;
  align-items: center;
  gap: 0.75rem;
}

.brand-icon {
  font-size: 1.5rem;
}

.brand-text {
  font-size: 1.25rem;
  font-weight: 700;
  color: white;
}

.sidebar-toggle {
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  padding: 0.5rem;
  border-radius: 6px;
  transition: all 0.2s ease;
}

.sidebar-toggle:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.sidebar-toggle svg {
  width: 16px;
  height: 16px;
}

/* User Profile */
.user-profile {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  position: relative;
}

.user-avatar-section {
  position: relative;
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

.online-indicator {
  position: absolute;
  bottom: -2px;
  right: -2px;
  width: 12px;
  height: 12px;
  background: #22c55e;
  border: 2px solid rgba(0, 0, 0, 0.8);
  border-radius: 50%;
}

.user-details {
  flex: 1;
  min-width: 0;
}

.user-name {
  font-weight: 600;
  color: white;
  font-size: 0.9rem;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.user-status {
  font-size: 0.75rem;
  color: #22c55e;
}

.user-menu-toggle {
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  padding: 0.25rem;
  border-radius: 4px;
  transition: all 0.2s ease;
}

.user-menu-toggle:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.user-menu-toggle svg {
  width: 14px;
  height: 14px;
}

/* Search */
.search-section {
  padding: 0 1rem 1rem;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.search-input-wrapper {
  position: relative;
  display: flex;
  align-items: center;
}

.search-input {
  width: 100%;
  height: 36px;
  background: rgba(255, 255, 255, 0.1);
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 18px;
  padding: 0 36px 0 36px;
  color: white;
  font-size: 0.85rem;
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
  left: 10px;
  width: 14px;
  height: 14px;
  color: rgba(255, 255, 255, 0.5);
  pointer-events: none;
}

.search-shortcut {
  position: absolute;
  right: 8px;
  background: rgba(255, 255, 255, 0.1);
  color: rgba(255, 255, 255, 0.6);
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 4px;
  padding: 2px 6px;
  font-size: 0.7rem;
  font-family: monospace;
  pointer-events: none;
}

/* Main Navigation */
.main-navigation {
  flex: 1;
  overflow-y: auto;
  padding: 0.5rem 0;
}

.nav-section {
  margin-bottom: 1.5rem;
}

.nav-section-bottom {
  margin-top: auto;
  margin-bottom: 0;
  padding-top: 1rem;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}

.nav-section-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 1rem 0.5rem;
  font-size: 0.75rem;
  font-weight: 600;
  color: rgba(255, 255, 255, 0.6);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.section-action {
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.4);
  cursor: pointer;
  padding: 0.25rem;
  border-radius: 4px;
  transition: all 0.2s ease;
}

.section-action:hover {
  color: rgba(255, 255, 255, 0.8);
  background: rgba(255, 255, 255, 0.1);
}

.section-action svg {
  width: 12px;
  height: 12px;
}

/* Navigation Items */
.nav-items {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding: 0.75rem 1rem;
  margin: 0 0.5rem;
  background: none;
  border: none;
  border-radius: 8px;
  color: rgba(255, 255, 255, 0.7);
  cursor: pointer;
  transition: all 0.2s ease;
  position: relative;
  text-align: left;
  width: calc(100% - 1rem);
}

.nav-item:hover {
  color: white;
  background: rgba(255, 255, 255, 0.1);
}

.nav-item.active {
  color: var(--accent-primary);
  background: rgba(var(--accent-primary-rgb), 0.15);
}

.nav-icon {
  font-size: 1.1rem;
  flex-shrink: 0;
  width: 20px;
  text-align: center;
}

.nav-label {
  font-weight: 500;
  font-size: 0.9rem;
  flex: 1;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.nav-badge {
  background: var(--accent-primary);
  color: white;
  font-size: 0.7rem;
  font-weight: 600;
  padding: 2px 6px;
  border-radius: 10px;
  min-width: 16px;
  text-align: center;
  line-height: 1;
}

.nav-badge.unread {
  background: #ef4444;
}

.nav-dot {
  position: absolute;
  top: 8px;
  right: 8px;
  width: 6px;
  height: 6px;
  background: var(--accent-primary);
  border-radius: 50%;
}

.nav-dot.unread {
  background: #ef4444;
}

/* Direct Messages */
.dm-item {
  padding: 0.5rem 1rem;
}

.dm-avatar {
  position: relative;
  width: 28px;
  height: 28px;
  flex-shrink: 0;
}

.dm-avatar img {
  width: 100%;
  height: 100%;
  border-radius: 50%;
  object-fit: cover;
}

.dm-avatar-placeholder {
  width: 100%;
  height: 100%;
  background: var(--accent-primary);
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  font-weight: 600;
  font-size: 0.8rem;
}

.dm-online-indicator {
  position: absolute;
  bottom: -1px;
  right: -1px;
  width: 10px;
  height: 10px;
  background: #22c55e;
  border: 2px solid rgba(0, 0, 0, 0.8);
  border-radius: 50%;
}

.dm-details {
  flex: 1;
  min-width: 0;
}

.dm-name {
  display: block;
  font-weight: 500;
  font-size: 0.85rem;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  color: inherit;
}

.dm-status {
  display: block;
  font-size: 0.75rem;
  color: rgba(255, 255, 255, 0.5);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 0.125rem;
}

/* User Menu Dropdown */
.user-menu-dropdown {
  position: absolute;
  bottom: 100%;
  left: 0;
  right: 0;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  margin-bottom: 0.5rem;
  overflow: hidden;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
}

.user-menu-header {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding: 1rem;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.user-menu-avatar img,
.user-menu-avatar .avatar-placeholder {
  width: 40px;
  height: 40px;
  border-radius: 50%;
}

.user-menu-avatar .avatar-placeholder {
  background: var(--accent-primary);
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  font-weight: 600;
}

.user-menu-info {
  flex: 1;
}

.user-menu-name {
  font-weight: 600;
  color: white;
  font-size: 0.9rem;
}

.user-menu-username {
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.6);
}

.user-menu-items {
  padding: 0.5rem 0;
}

.user-menu-item {
  display: flex;
  align-items: center;
  gap: 0.75rem;
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

.user-menu-item:hover {
  background: rgba(255, 255, 255, 0.1);
  color: white;
}

.user-menu-item.logout {
  color: #ef4444;
}

.user-menu-item.logout:hover {
  background: rgba(239, 68, 68, 0.1);
}

.menu-icon {
  font-size: 0.9rem;
  opacity: 0.8;
}

.menu-divider {
  border: none;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  margin: 0.5rem 0;
}

/* Mobile */
.mobile-backdrop {
  display: none;
}

@media (max-width: 768px) {
  .sidebar-navigation {
    transform: translateX(-100%);
    transition: transform 0.3s ease;
  }
  
  .sidebar-navigation.mobile-open {
    transform: translateX(0);
  }
  
  .mobile-backdrop {
    display: block;
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.5);
    z-index: 999;
  }
}

/* Transitions */
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.3s ease;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.slide-up-enter-active,
.slide-up-leave-active {
  transition: all 0.3s cubic-bezier(0.4, 0, 0.2, 1);
}

.slide-up-enter-from,
.slide-up-leave-to {
  opacity: 0;
  transform: translateY(10px);
}
</style>
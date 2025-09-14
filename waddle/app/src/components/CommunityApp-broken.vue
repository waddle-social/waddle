<template>
  <div class="community-app">
    <!-- Enhanced App with Error Boundary -->
    <ErrorBoundary @retry="handleRetry">
      <Suspense>
        <template #default>
          <div class="app-content">
            <!-- Sidebar Navigation -->
            <SidebarNavigation
              :user="mockUser"
              :current-view="currentView"
              :collapsed="sidebarCollapsed"
              @switch-view="handleSwitchView"
              @search="handleGlobalSearch"
              @logout="handleLogout"
              @toggle-sidebar="sidebarCollapsed = !sidebarCollapsed"
            />

            <!-- Main Content Area -->
            <main :class="['main-content', { 'sidebar-collapsed': sidebarCollapsed }]">
              <!-- Custom Feeds Bar -->
              <CustomFeedsBar
                :custom-feeds="customFeeds"
                :active-feed-id="activeFeedId"
                @select-feed="handleSelectFeed"
                @create-feed="handleCreateFeed"
                @remove-feed="handleRemoveFeed"
              />
              
              <!-- Content Views -->
              <div class="content-container">
                <!-- Feed View - The Firehose -->
                <div v-if="currentView === 'feed'" class="content-view feed-view">
                  <div class="view-header">
                    <h1 class="view-title">🔥 Feed</h1>
                    <p class="view-description">Your personalized firehose of community content</p>
                  </div>
              
              <div class="feed-content">
                <div v-for="item in mockFeedItems" :key="item.content.id" class="feed-item">
                  <ContentItemComponent
                    :item="item"
                    :layout="currentLayout"
                    :interactive="true"
                    :bookmarked-items="bookmarkedItems"
                    :pinned-items="pinnedItems"
                    @react="handleContentReaction"
                    @bookmark="handleContentBookmark"
                    @pin="handleContentPin"
                    @share="handleContentShare"
                    @vote="handleContentVote"
                    @rsvp="handleEventRSVP"
                    @follow="handleUserFollow"
                    @join="handleHangoutJoin"
                    @direct-message="handleDirectMessage"
                    @comment="handleContentComment"
                    @connect="handleUserConnect"
                    @hide="handleContentHide"
                    @report="handleContentReport"
                  />
                </div>
              </div>
            </div>

            <!-- Chat View -->
            <div v-else-if="currentView === 'chat'" class="content-view chat-view">
              <div class="view-header">
                <h1 class="view-title">💬 Chat</h1>
                <p class="view-description">Community conversations in real-time</p>
              </div>
              
              <div class="chat-content">
                <EnhancedChatRoom :username="username" />
              </div>
            </div>

            <!-- Events View -->
            <div v-else-if="currentView === 'events'" class="content-view events-view">
              <div class="view-header">
                <h1 class="view-title">📅 Events</h1>
                <p class="view-description">Discover and organize community events</p>
              </div>
              
              <div class="events-content">
                <div class="coming-soon">
                  <h3>🚧 Coming Soon</h3>
                  <p>Event management and discovery features are being built!</p>
                </div>
              </div>
            </div>

            <!-- People View -->
            <div v-else-if="currentView === 'people'" class="content-view people-view">
              <div class="view-header">
                <h1 class="view-title">👥 People</h1>
                <p class="view-description">Connect with community members</p>
              </div>
              
              <div class="people-content">
                <div class="coming-soon">
                  <h3>🚧 Coming Soon</h3>
                  <p>People directory and social connections are being built!</p>
                </div>
              </div>
            </div>

            <!-- Links View -->
            <div v-else-if="currentView === 'links'" class="content-view links-view">
              <div class="view-header">
                <h1 class="view-title">🔗 Links</h1>
                <p class="view-description">Share and discover interesting links</p>
              </div>
              
              <div class="links-content">
                <div class="coming-soon">
                  <h3>🚧 Coming Soon</h3>
                  <p>Reddit-style link sharing system is being built!</p>
                </div>
              </div>
            </div>

            <!-- Hangouts View -->
            <div v-else-if="currentView === 'hangouts'" class="content-view hangouts-view">
              <div class="view-header">
                <h1 class="view-title">🎧 Hangouts</h1>
                <p class="view-description">Audio, video, and live streaming spaces</p>
              </div>
              
              <div class="hangouts-content">
                <div class="coming-soon">
                  <h3>🚧 Coming Soon</h3>
                  <p>Voice, video, and streaming hangouts are being built!</p>
                </div>
              </div>
            </div>

            <!-- Messages View -->
            <div v-else-if="currentView === 'messages'" class="content-view messages-view">
              <div class="view-header">
                <h1 class="view-title">💌 Messages</h1>
                <p class="view-description">Private conversations and group chats</p>
              </div>
              
              <div class="messages-content">
                <div class="coming-soon">
                  <h3>🚧 Coming Soon</h3>
                  <p>Direct messaging and group chats are being built!</p>
                </div>
              </div>
            </div>
          </div>
        </main>

        <!-- Development Tools -->
          <div v-if="isDevelopment" class="dev-tools">
            <button @click="toggleDevTools" class="dev-toggle">
              {{ showDevTools ? '🔧 Hide Tools' : '🔧 Show Tools' }}
            </button>
            
            <div v-if="showDevTools" class="dev-panel">
              <div class="dev-section">
                <h4>Current State</h4>
                <div class="dev-info">
                  <div>View: {{ currentView }}</div>
                  <div>Layout: {{ currentLayout }}</div>
                  <div>Real-time: {{ realTimeEnabled ? 'ON' : 'OFF' }}</div>
                  <div>Feed Items: {{ mockFeedItems.length }}</div>
                </div>
              </div>
              
              <div class="dev-section">
                <h4>Quick Actions</h4>
                <div class="dev-actions">
                  <button @click="addMockContent">Add Mock Content</button>
                  <button @click="clearFeed">Clear Feed</button>
                  <button @click="simulateNotification">Test Notification</button>
                </div>
              </div>
            </div>
          </div>
          </div>
        </template>

        <template #fallback>
          <LoadingSkeleton type="chatroom" />
        </template>
      </Suspense>
    </ErrorBoundary>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useActor } from '@xstate/vue'
import { enhancedAppMachine, createAppMachineInput, createNotification } from '../machines/enhancedAppMachine'
import type { ViewType, CustomView, FeedItem, ContentItem, LayoutMode, User, CustomFeed } from '../types/content'

// Import components
import ErrorBoundary from './ErrorBoundary.vue'
import LoadingSkeleton from './LoadingSkeleton.vue'
import SidebarNavigation from './SidebarNavigation.vue'
import CustomFeedsBar from './CustomFeedsBar.vue'
import ContentItemComponent from './ContentItem.vue'
import EnhancedChatRoom from './EnhancedChatRoom.vue'

interface Props {
  username: string
  initialView?: ViewType
}

const props = withDefaults(defineProps<Props>(), {
  initialView: 'feed'
})

// Mock user data (in real app, this would come from authentication)
const mockUser = computed<User>(() => ({
  id: 'user_123',
  username: props.username,
  displayName: props.username.charAt(0).toUpperCase() + props.username.slice(1),
  email: `${props.username}@example.com`,
  avatar: undefined,
  bio: `Hello, I'm ${props.username}!`,
  location: 'Internet',
  joinedAt: Date.now() - (Math.random() * 365 * 24 * 60 * 60 * 1000), // Random join date within last year
  preferences: {
    defaultView: props.initialView,
    customViews: [],
    notifications: {
      mentions: true,
      directMessages: true,
      events: true,
      following: true,
      trending: true,
    },
    privacy: {
      showOnlineStatus: true,
      allowDirectMessages: 'everyone',
      profileVisibility: 'public',
    },
    content: {
      autoplayVideos: false,
      showNSFW: false,
      compactMode: false,
    },
  },
}))

// Initialize app machine
const appActor = useActor(enhancedAppMachine, {
  input: createAppMachineInput(mockUser.value, props.initialView)
})

// Reactive state from machine
const currentView = computed(() => appActor.snapshot.value.context.currentView)
const activeCustomView = computed(() => appActor.snapshot.value.context.activeCustomView)
const customViews = computed(() => appActor.snapshot.value.context.customViews)
const currentLayout = computed(() => appActor.snapshot.value.context.currentLayout)
const realTimeEnabled = computed(() => appActor.snapshot.value.context.realTimeEnabled)

// Local UI state
const isDevelopment = ref(import.meta.env.DEV)
const showDevTools = ref(false)
const bookmarkedItems = ref(new Set<string>())
const pinnedItems = ref(new Set<string>())
const sidebarCollapsed = ref(false)

// Custom feeds state
const customFeeds = ref<CustomFeed[]>([
  {
    id: 'all',
    name: 'All Posts',
    emoji: '🌟',
    color: '#3b82f6',
    contentTypes: ['chat', 'event', 'link', 'person', 'hangout', 'message'],
    keywords: [],
    filters: {
      contentTypes: ['chat', 'event', 'link', 'person', 'hangout', 'message'],
      keywords: []
    },
    unreadCount: 42
  },
  {
    id: 'tech',
    name: 'Tech Talk',
    emoji: '💻',
    color: '#10b981',
    contentTypes: ['chat', 'link'],
    keywords: ['javascript', 'vue', 'react', 'nodejs'],
    filters: {
      contentTypes: ['chat', 'link'],
      keywords: ['javascript', 'vue', 'react', 'nodejs']
    },
    unreadCount: 7
  },
  {
    id: 'events',
    name: 'Events',
    emoji: '📅',
    color: '#f59e0b',
    contentTypes: ['event'],
    keywords: [],
    filters: {
      contentTypes: ['event'],
      keywords: []
    },
    unreadCount: 3
  }
])
const activeFeedId = ref('all')

// Mock feed data
const mockFeedItems = ref<FeedItem[]>([
  {
    content: {
      id: 'chat_1',
      type: 'chat',
      userId: 'user_456',
      username: 'alice',
      timestamp: Date.now() - 300000,
      visibility: 'public',
      content: 'Hey everyone! Just joined the community. Excited to be here! 🎉',
      category: 'General',
    } as ContentItem,
    score: 0.8,
    isTrending: true,
  },
  {
    content: {
      id: 'event_1',
      type: 'event',
      userId: 'user_789',
      username: 'bob',
      timestamp: Date.now() - 600000,
      visibility: 'public',
      title: 'Weekly Tech Meetup',
      description: 'Join us for our weekly discussion about the latest in tech and programming!',
      startTime: Date.now() + 86400000,
      endTime: Date.now() + 86400000 + 7200000,
      location: {
        type: 'virtual',
        virtualUrl: 'https://meet.example.com/tech-meetup'
      },
      rsvpStatus: 'none',
      attendeeCount: 12,
      maxAttendees: 50,
    } as ContentItem,
    score: 0.9,
    isTrending: false,
  },
  {
    content: {
      id: 'link_1',
      type: 'link',
      userId: 'user_101',
      username: 'charlie',
      timestamp: Date.now() - 900000,
      visibility: 'public',
      title: 'Amazing New Vue.js 3.4 Features You Should Know',
      url: 'https://vuejs.org/blog/vue-3-4.html',
      description: 'Vue 3.4 brings some incredible new features including better performance and DX improvements.',
      domain: 'vuejs.org',
      votes: 42,
      commentCount: 8,
    } as ContentItem,
    score: 0.7,
    isTrending: false,
  },
])

// Event handlers
const handleSwitchView = (view: ViewType) => {
  appActor.send({ type: 'NAVIGATE_TO', view })
}

const handleSwitchToCustomView = (view: CustomView) => {
  appActor.send({ type: 'SWITCH_TO_CUSTOM_VIEW', view })
}

const handleGlobalSearch = (query: string) => {
  appActor.send({ type: 'GLOBAL_SEARCH', query })
}

const handleToggleRealTime = () => {
  appActor.send({ type: 'TOGGLE_REAL_TIME' })
}

const handleChangeLayout = (layout: LayoutMode) => {
  appActor.send({ type: 'SET_LAYOUT', layout })
}

const handleCreateCustomView = () => {
  // In real app, this would open a modal/dialog
  const viewName = prompt('Enter name for custom view:')
  if (viewName) {
    const customView: CustomView = {
      id: `view_${Date.now()}`,
      name: viewName,
      contentTypes: ['chat', 'event'],
      filters: {
        categories: [],
        users: [],
        timeRange: { preset: 'day' },
        keywords: [],
        tags: [],
      },
      layout: 'feed',
      sortBy: 'timestamp',
      sortOrder: 'desc',
    }
    appActor.send({ type: 'CREATE_CUSTOM_VIEW', view: customView })
  }
}

const handleEditCustomView = (view: CustomView) => {
  alert(`Editing custom view: ${view.name}`)
}

const handleLogout = () => {
  sessionStorage.removeItem('username')
  window.location.href = '/'
}

// Content interaction handlers
const handleContentReaction = (contentId: string, reaction: string) => {
  console.log('React to content:', contentId, reaction)
}

const handleContentBookmark = (contentId: string, bookmarked: boolean) => {
  if (bookmarked) {
    bookmarkedItems.value.add(contentId)
  } else {
    bookmarkedItems.value.delete(contentId)
  }
}

const handleContentPin = (contentId: string, pinned: boolean) => {
  if (pinned) {
    pinnedItems.value.add(contentId)
  } else {
    pinnedItems.value.delete(contentId)
  }
}

const handleContentShare = (contentId: string) => {
  navigator.clipboard?.writeText(`${window.location.origin}/content/${contentId}`)
  alert('Content link copied to clipboard!')
}

const handleContentVote = (contentId: string, direction: 'up' | 'down') => {
  console.log('Vote on content:', contentId, direction)
}

const handleEventRSVP = (eventId: string, status: 'going' | 'maybe' | 'not_going') => {
  console.log('RSVP to event:', eventId, status)
}

const handleUserFollow = (userId: string, following: boolean) => {
  console.log('Follow user:', userId, following)
}

const handleHangoutJoin = (hangoutId: string) => {
  console.log('Join hangout:', hangoutId)
}

const handleDirectMessage = (userId: string) => {
  console.log('Send DM to user:', userId)
}

const handleContentComment = (contentId: string) => {
  console.log('Comment on content:', contentId)
}

const handleUserConnect = (userId: string) => {
  console.log('Connect with user:', userId)
}

const handleContentHide = (contentId: string) => {
  console.log('Hide content:', contentId)
}

const handleContentReport = (contentId: string, reason: string) => {
  console.log('Report content:', contentId, reason)
}

const handleRetry = () => {
  appActor.send({ type: 'RETRY_OPERATION' })
}

// Custom feeds handlers
const handleSelectFeed = (feedId: string) => {
  activeFeedId.value = feedId
  // Filter content based on selected feed
  const selectedFeed = customFeeds.value.find(f => f.id === feedId)
  if (selectedFeed) {
    // In real app, this would trigger feed refresh with filters
    console.log('Switching to feed:', selectedFeed.name, selectedFeed.filters)
  }
}

const handleCreateFeed = (feedData: Omit<CustomFeed, 'id' | 'unreadCount'>) => {
  const newFeed: CustomFeed = {
    ...feedData,
    id: `feed-${Date.now()}`,
    unreadCount: 0
  }
  customFeeds.value.push(newFeed)
  console.log('Created new feed:', newFeed)
}

const handleRemoveFeed = (feedId: string) => {
  const index = customFeeds.value.findIndex(f => f.id === feedId)
  if (index > -1 && feedId !== 'all') {
    customFeeds.value.splice(index, 1)
    if (activeFeedId.value === feedId) {
      activeFeedId.value = 'all'
    }
    console.log('Removed feed:', feedId)
  }
}

// Development tools
const toggleDevTools = () => {
  showDevTools.value = !showDevTools.value
}

const addMockContent = () => {
  const newItem: FeedItem = {
    content: {
      id: `mock_${Date.now()}`,
      type: 'chat',
      userId: 'mock_user',
      username: 'MockBot',
      timestamp: Date.now(),
      visibility: 'public',
      content: `Mock content added at ${new Date().toLocaleTimeString()}`,
      category: 'Tech',
    } as ContentItem,
    score: Math.random(),
    isTrending: Math.random() > 0.7,
  }
  mockFeedItems.value.unshift(newItem)
}

const clearFeed = () => {
  mockFeedItems.value = []
}

const simulateNotification = () => {
  const notification = createNotification(
    'system',
    'Test Notification',
    'This is a test notification from the development tools.'
  )
  appActor.send({ type: 'ADD_NOTIFICATION', notification })
}

onMounted(() => {
  // Add welcome notification
  setTimeout(() => {
    const welcomeNotification = createNotification(
      'system',
      'Welcome to Waddle!',
      `Welcome back, ${props.username}! Explore the new community features.`
    )
    appActor.send({ type: 'ADD_NOTIFICATION', notification: welcomeNotification })
  }, 2000)
})
</script>

<style scoped>
.community-app {
  min-height: 100vh;
  background: linear-gradient(135deg, #000 0%, #1a1a1a 100%);
  color: white;
}

.app-content {
  display: flex;
  min-height: 100vh;
}

.main-content {
  flex: 1;
  display: flex;
  flex-direction: column;
  margin-left: 280px;
  transition: margin-left 0.3s ease;
  min-width: 0;
}

.main-content.sidebar-collapsed {
  margin-left: 80px;
}

.content-view {
  max-width: 1200px;
  margin: 0 auto;
  padding: 2rem;
}

.view-header {
  text-align: center;
  margin-bottom: 2rem;
  padding-bottom: 1rem;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.view-title {
  font-size: 2.5rem;
  font-weight: 700;
  margin: 0 0 0.5rem 0;
  background: linear-gradient(135deg, var(--accent-primary) 0%, #a855f7 100%);
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
  background-clip: text;
}

.view-description {
  font-size: 1.1rem;
  color: rgba(255, 255, 255, 0.7);
  margin: 0;
}

.feed-content {
  display: flex;
  flex-direction: column;
  gap: 1.5rem;
}

.feed-item {
  animation: fadeInUp 0.5s ease-out;
}

@keyframes fadeInUp {
  from {
    opacity: 0;
    transform: translateY(20px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.chat-content {
  background: rgba(255, 255, 255, 0.02);
  border-radius: 16px;
  overflow: hidden;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.coming-soon {
  text-align: center;
  padding: 4rem 2rem;
  background: rgba(255, 255, 255, 0.02);
  border-radius: 16px;
  border: 1px dashed rgba(255, 255, 255, 0.2);
}

.coming-soon h3 {
  font-size: 1.5rem;
  margin: 0 0 1rem 0;
  color: var(--accent-primary);
}

.coming-soon p {
  font-size: 1rem;
  color: rgba(255, 255, 255, 0.7);
  margin: 0;
}

/* Development Tools */
.dev-tools {
  position: fixed;
  bottom: 1rem;
  left: 1rem;
  z-index: 1000;
}

.dev-toggle {
  background: rgba(0, 0, 0, 0.8);
  border: 1px solid var(--accent-primary);
  color: var(--accent-primary);
  padding: 0.5rem 1rem;
  border-radius: 8px;
  cursor: pointer;
  font-size: 0.8rem;
  font-weight: 500;
  transition: all 0.2s ease;
}

.dev-toggle:hover {
  background: rgba(var(--accent-primary-rgb), 0.1);
}

.dev-panel {
  position: absolute;
  bottom: 100%;
  left: 0;
  width: 300px;
  background: rgba(0, 0, 0, 0.95);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  padding: 1rem;
  margin-bottom: 0.5rem;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
}

.dev-section {
  margin-bottom: 1rem;
}

.dev-section:last-child {
  margin-bottom: 0;
}

.dev-section h4 {
  color: var(--accent-primary);
  font-size: 0.9rem;
  margin: 0 0 0.5rem 0;
  font-weight: 600;
}

.dev-info {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
  font-size: 0.8rem;
  color: rgba(255, 255, 255, 0.7);
}

.dev-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.dev-actions button {
  background: none;
  border: 1px solid rgba(255, 255, 255, 0.2);
  color: rgba(255, 255, 255, 0.8);
  padding: 0.25rem 0.5rem;
  border-radius: 4px;
  cursor: pointer;
  font-size: 0.7rem;
  transition: all 0.2s ease;
}

.dev-actions button:hover {
  border-color: var(--accent-primary);
  color: var(--accent-primary);
}

/* Responsive Design */
@media (max-width: 768px) {
  .content-view {
    padding: 1rem;
  }
  
  .main-content {
    padding-top: 100px;
  }
  
  .view-title {
    font-size: 2rem;
  }
  
  .dev-tools {
    bottom: 0.5rem;
    left: 0.5rem;
  }
  
  .dev-panel {
    width: calc(100vw - 1rem);
    max-width: 280px;
  }
}
</style>
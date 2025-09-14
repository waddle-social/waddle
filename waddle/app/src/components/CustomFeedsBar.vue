<template>
  <div class="custom-feeds-bar">
    <div class="feeds-container">
      <!-- Active Feeds -->
      <div class="feeds-list">
        <button
          v-for="feed in customFeeds"
          :key="feed.id"
          :class="['feed-tab', { active: activeFeedId === feed.id }]"
          @click="selectFeed(feed.id)"
        >
          <div class="feed-icon" :style="{ background: feed.color }">
            {{ feed.emoji || feed.name.charAt(0).toUpperCase() }}
          </div>
          <span class="feed-name">{{ feed.name }}</span>
          <div v-if="feed.unreadCount > 0" class="unread-badge">
            {{ formatCount(feed.unreadCount) }}
          </div>
          <button
            v-if="feed.id !== 'all'"
            @click.stop="removeFeed(feed.id)"
            class="remove-feed"
            title="Remove feed"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <line x1="18" y1="6" x2="6" y2="18"></line>
              <line x1="6" y1="6" x2="18" y2="18"></line>
            </svg>
          </button>
        </button>
      </div>

      <!-- Add New Feed -->
      <div class="feed-actions">
        <button @click="showFeedBuilder = true" class="add-feed-btn" title="Create custom feed">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <line x1="12" y1="5" x2="12" y2="19"></line>
            <line x1="5" y1="12" x2="19" y2="12"></line>
          </svg>
          <span>Add Feed</span>
        </button>

        <button @click="toggleFeedBuilder" class="filter-btn" title="Quick filters">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3"></polygon>
          </svg>
        </button>
      </div>
    </div>

    <!-- Feed Builder Modal -->
    <Teleport to="body">
      <div v-if="showFeedBuilder" class="feed-builder-modal" @click="closeFeedBuilder">
        <div class="feed-builder" @click.stop>
          <header class="builder-header">
            <h3>Create Custom Feed</h3>
            <button @click="closeFeedBuilder" class="close-btn">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
                <line x1="18" y1="6" x2="6" y2="18"></line>
                <line x1="6" y1="6" x2="18" y2="18"></line>
              </svg>
            </button>
          </header>

          <form @submit.prevent="createFeed" class="builder-form">
            <div class="form-group">
              <label>Feed Name</label>
              <input
                v-model="newFeed.name"
                type="text"
                placeholder="My Custom Feed"
                required
              />
            </div>

            <div class="form-group">
              <label>Emoji/Icon</label>
              <input
                v-model="newFeed.emoji"
                type="text"
                placeholder="🔥"
                maxlength="2"
              />
            </div>

            <div class="form-group">
              <label>Color</label>
              <div class="color-picker">
                <input v-model="newFeed.color" type="color" />
                <span>{{ newFeed.color }}</span>
              </div>
            </div>

            <div class="form-group">
              <label>Content Types</label>
              <div class="content-types">
                <label v-for="type in contentTypes" :key="type.value" class="checkbox-label">
                  <input
                    v-model="newFeed.contentTypes"
                    :value="type.value"
                    type="checkbox"
                  />
                  <span>{{ type.label }}</span>
                </label>
              </div>
            </div>

            <div class="form-group">
              <label>Keywords (optional)</label>
              <input
                v-model="keywordInput"
                @keydown.enter.prevent="addKeyword"
                type="text"
                placeholder="Type keyword and press Enter"
              />
              <div v-if="newFeed.keywords.length > 0" class="keywords-list">
                <span
                  v-for="keyword in newFeed.keywords"
                  :key="keyword"
                  class="keyword-tag"
                >
                  {{ keyword }}
                  <button @click="removeKeyword(keyword)" type="button">×</button>
                </span>
              </div>
            </div>

            <div class="form-actions">
              <button type="button" @click="closeFeedBuilder" class="cancel-btn">
                Cancel
              </button>
              <button type="submit" class="create-btn">
                Create Feed
              </button>
            </div>
          </form>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed } from 'vue'
import type { CustomFeed, ContentType } from '../types/content'

interface Props {
  customFeeds: CustomFeed[]
  activeFeedId: string
}

const props = defineProps<Props>()

const emit = defineEmits<{
  selectFeed: [feedId: string]
  createFeed: [feed: Omit<CustomFeed, 'id' | 'unreadCount'>]
  removeFeed: [feedId: string]
}>()

const showFeedBuilder = ref(false)
const keywordInput = ref('')

const newFeed = ref({
  name: '',
  emoji: '📋',
  color: '#3b82f6',
  contentTypes: [] as ContentType[],
  keywords: [] as string[]
})

const contentTypes = [
  { value: 'chat' as ContentType, label: 'Chat Messages' },
  { value: 'event' as ContentType, label: 'Events' },
  { value: 'link' as ContentType, label: 'Links' },
  { value: 'person' as ContentType, label: 'People' },
  { value: 'hangout' as ContentType, label: 'Hangouts' },
  { value: 'message' as ContentType, label: 'Direct Messages' }
]

const formatCount = (count: number) => {
  if (count < 1000) return count.toString()
  if (count < 1000000) return (count / 1000).toFixed(1) + 'k'
  return (count / 1000000).toFixed(1) + 'M'
}

const selectFeed = (feedId: string) => {
  emit('selectFeed', feedId)
}

const removeFeed = (feedId: string) => {
  emit('removeFeed', feedId)
}

const toggleFeedBuilder = () => {
  showFeedBuilder.value = !showFeedBuilder.value
}

const closeFeedBuilder = () => {
  showFeedBuilder.value = false
  resetForm()
}

const addKeyword = () => {
  const keyword = keywordInput.value.trim()
  if (keyword && !newFeed.value.keywords.includes(keyword)) {
    newFeed.value.keywords.push(keyword)
    keywordInput.value = ''
  }
}

const removeKeyword = (keyword: string) => {
  const index = newFeed.value.keywords.indexOf(keyword)
  if (index > -1) {
    newFeed.value.keywords.splice(index, 1)
  }
}

const createFeed = () => {
  if (newFeed.value.name.trim()) {
    emit('createFeed', {
      name: newFeed.value.name.trim(),
      emoji: newFeed.value.emoji,
      color: newFeed.value.color,
      contentTypes: newFeed.value.contentTypes,
      keywords: newFeed.value.keywords,
      filters: {
        contentTypes: newFeed.value.contentTypes,
        keywords: newFeed.value.keywords
      }
    })
    closeFeedBuilder()
  }
}

const resetForm = () => {
  newFeed.value = {
    name: '',
    emoji: '📋',
    color: '#3b82f6',
    contentTypes: [],
    keywords: []
  }
  keywordInput.value = ''
}
</script>

<style scoped>
.custom-feeds-bar {
  background: rgba(0, 0, 0, 0.3);
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
  backdrop-filter: blur(10px);
}

.feeds-container {
  display: flex;
  align-items: center;
  padding: 0.75rem 1rem;
  gap: 1rem;
  max-width: 100%;
  overflow-x: auto;
}

.feeds-list {
  display: flex;
  gap: 0.5rem;
  flex: 1;
  min-width: 0;
}

.feed-tab {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.5rem 0.75rem;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  color: rgba(255, 255, 255, 0.8);
  cursor: pointer;
  transition: all 0.2s ease;
  white-space: nowrap;
  position: relative;
  min-width: 0;
}

.feed-tab:hover {
  background: rgba(255, 255, 255, 0.1);
  border-color: rgba(255, 255, 255, 0.2);
  color: white;
}

.feed-tab.active {
  background: var(--accent-primary);
  border-color: var(--accent-primary);
  color: white;
}

.feed-icon {
  width: 20px;
  height: 20px;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 0.7rem;
  color: white;
  flex-shrink: 0;
}

.feed-name {
  font-size: 0.8rem;
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
}

.unread-badge {
  background: #ef4444;
  color: white;
  font-size: 0.6rem;
  padding: 0.125rem 0.375rem;
  border-radius: 10px;
  min-width: 18px;
  text-align: center;
  line-height: 1.2;
  flex-shrink: 0;
}

.remove-feed {
  width: 16px;
  height: 16px;
  padding: 0;
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.5);
  cursor: pointer;
  border-radius: 2px;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.remove-feed:hover {
  background: rgba(239, 68, 68, 0.2);
  color: #ef4444;
}

.remove-feed svg {
  width: 12px;
  height: 12px;
}

.feed-actions {
  display: flex;
  gap: 0.5rem;
  flex-shrink: 0;
}

.add-feed-btn,
.filter-btn {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.5rem 0.75rem;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 8px;
  color: rgba(255, 255, 255, 0.8);
  cursor: pointer;
  transition: all 0.2s ease;
  font-size: 0.8rem;
  font-weight: 500;
}

.add-feed-btn:hover,
.filter-btn:hover {
  background: rgba(255, 255, 255, 0.1);
  color: white;
}

.add-feed-btn svg,
.filter-btn svg {
  width: 16px;
  height: 16px;
}

.feed-builder-modal {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(0, 0, 0, 0.7);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
  backdrop-filter: blur(5px);
}

.feed-builder {
  background: rgba(20, 20, 30, 0.95);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  width: 100%;
  max-width: 500px;
  max-height: 80vh;
  overflow-y: auto;
  backdrop-filter: blur(20px);
}

.builder-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 1rem 1.5rem;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.builder-header h3 {
  color: white;
  font-size: 1.125rem;
  font-weight: 600;
  margin: 0;
}

.close-btn {
  width: 32px;
  height: 32px;
  padding: 0;
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.6);
  cursor: pointer;
  border-radius: 6px;
  display: flex;
  align-items: center;
  justify-content: center;
}

.close-btn:hover {
  background: rgba(255, 255, 255, 0.1);
  color: white;
}

.close-btn svg {
  width: 18px;
  height: 18px;
}

.builder-form {
  padding: 1.5rem;
}

.form-group {
  margin-bottom: 1.5rem;
}

.form-group label {
  display: block;
  color: rgba(255, 255, 255, 0.8);
  font-size: 0.9rem;
  font-weight: 500;
  margin-bottom: 0.5rem;
}

.form-group input[type="text"] {
  width: 100%;
  padding: 0.75rem;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 8px;
  color: white;
  font-size: 0.9rem;
}

.form-group input[type="text"]:focus {
  outline: none;
  border-color: var(--accent-primary);
  background: rgba(255, 255, 255, 0.1);
}

.color-picker {
  display: flex;
  align-items: center;
  gap: 0.75rem;
}

.color-picker input[type="color"] {
  width: 40px;
  height: 40px;
  border: none;
  border-radius: 6px;
  cursor: pointer;
}

.color-picker span {
  color: rgba(255, 255, 255, 0.7);
  font-family: 'Monaco', 'Menlo', monospace;
  font-size: 0.8rem;
}

.content-types {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 0.75rem;
  margin-top: 0.5rem;
}

.checkbox-label {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  color: rgba(255, 255, 255, 0.8);
  font-size: 0.8rem;
  cursor: pointer;
}

.checkbox-label input[type="checkbox"] {
  width: 16px;
  height: 16px;
  accent-color: var(--accent-primary);
}

.keywords-list {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
  margin-top: 0.5rem;
}

.keyword-tag {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.25rem 0.5rem;
  background: rgba(var(--accent-primary-rgb), 0.2);
  color: var(--accent-primary);
  border-radius: 12px;
  font-size: 0.75rem;
  font-weight: 500;
}

.keyword-tag button {
  background: none;
  border: none;
  color: currentColor;
  cursor: pointer;
  font-size: 0.9rem;
  padding: 0;
  width: 16px;
  height: 16px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 2px;
}

.keyword-tag button:hover {
  background: rgba(239, 68, 68, 0.2);
  color: #ef4444;
}

.form-actions {
  display: flex;
  gap: 0.75rem;
  justify-content: flex-end;
  padding-top: 1rem;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}

.cancel-btn,
.create-btn {
  padding: 0.75rem 1.5rem;
  border-radius: 8px;
  font-size: 0.9rem;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
}

.cancel-btn {
  background: none;
  border: 1px solid rgba(255, 255, 255, 0.2);
  color: rgba(255, 255, 255, 0.8);
}

.cancel-btn:hover {
  background: rgba(255, 255, 255, 0.05);
  color: white;
}

.create-btn {
  background: var(--accent-primary);
  border: 1px solid var(--accent-primary);
  color: white;
}

.create-btn:hover {
  background: var(--accent-hover);
  border-color: var(--accent-hover);
}

@media (max-width: 768px) {
  .feeds-container {
    padding: 0.5rem;
  }

  .feed-tab .feed-name {
    display: none;
  }

  .add-feed-btn span {
    display: none;
  }

  .feed-builder {
    margin: 1rem;
    max-width: none;
  }
}
</style>
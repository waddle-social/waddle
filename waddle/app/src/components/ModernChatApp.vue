<template>
  <!-- Error Boundary wraps everything -->
  <ErrorBoundary @retry="handleRetry">
    <!-- Suspense for async loading -->
    <Suspense>
      <!-- Async Content -->
      <template #default>
        <div class="modern-chat-app">
          <!-- Intersection Observer Demo -->
          <div ref="headerRef" class="header-section">
            <Transition name="slide-down" appear>
              <EnhancedChatRoom
                v-if="isHeaderVisible"
                :username="username"
                @connection-status="handleConnectionStatus"
              />
            </Transition>
          </div>

          <!-- XState Inspector Panel (Development Only) -->
          <div v-if="showInspector && isDevelopment" class="fixed top-4 right-4 z-50">
            <div class="bg-black/80 backdrop-blur-xl border border-white/20 rounded-lg p-4 text-white text-sm">
              <div class="flex items-center justify-between mb-2">
                <span class="font-semibold">XState Inspector</span>
                <button @click="showInspector = false" class="text-white/60 hover:text-white">×</button>
              </div>
              <div class="text-xs text-white/70">
                <div>Machines: {{ activeMachines.length }}</div>
                <div>Status: {{ connectionStatus }}</div>
              </div>
              <button
                @click="openInspector"
                class="mt-2 px-3 py-1 bg-accent-primary hover:bg-accent-primary-dark rounded text-xs"
              >
                Open Inspector
              </button>
            </div>
          </div>

          <!-- Performance Monitor (Development Only) -->
          <div v-if="showPerformanceMonitor && isDevelopment" class="fixed bottom-4 right-4 z-50">
            <div class="bg-black/80 backdrop-blur-xl border border-white/20 rounded-lg p-3 text-white text-xs">
              <div class="flex items-center justify-between mb-2">
                <span class="font-semibold">Performance</span>
                <button @click="showPerformanceMonitor = false" class="text-white/60 hover:text-white">×</button>
              </div>
              <div class="space-y-1 text-white/70">
                <div>Render Time: {{ renderTime }}ms</div>
                <div>Memory: {{ memoryUsage }}MB</div>
                <div>FPS: {{ fps }}</div>
                <div>Messages: {{ messageCount }}</div>
              </div>
            </div>
          </div>

          <!-- Toast Notifications -->
          <TransitionGroup name="toast" tag="div" class="fixed top-4 left-4 z-50 space-y-2">
            <div
              v-for="toast in toasts"
              :key="toast.id"
              :class="[
                'px-4 py-3 rounded-lg backdrop-blur-xl border shadow-lg text-sm',
                toast.type === 'success' && 'bg-green-500/20 border-green-400/30 text-green-300',
                toast.type === 'error' && 'bg-red-500/20 border-red-400/30 text-red-300',
                toast.type === 'info' && 'bg-blue-500/20 border-blue-400/30 text-blue-300',
                toast.type === 'warning' && 'bg-yellow-500/20 border-yellow-400/30 text-yellow-300',
              ]"
            >
              {{ toast.message }}
            </div>
          </TransitionGroup>
        </div>
      </template>

      <!-- Loading Fallback -->
      <template #fallback>
        <LoadingSkeleton type="chatroom" />
      </template>
    </Suspense>
  </ErrorBoundary>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed } from 'vue'
import { useIntersectionObserver } from '../composables/useIntersectionObserver'
import { initializeXStateInspector } from '../utils/xstateInspector'
import ErrorBoundary from './ErrorBoundary.vue'
import LoadingSkeleton from './LoadingSkeleton.vue'
import EnhancedChatRoom from './EnhancedChatRoom.vue'

interface Props {
  username: string
}

const props = defineProps<Props>()

// Reactive state
const isDevelopment = ref(import.meta.env.DEV)
const showInspector = ref(false)
const showPerformanceMonitor = ref(false)
const connectionStatus = ref('disconnected')
const activeMachines = ref<string[]>([])
const toasts = ref<Array<{
  id: string
  type: 'success' | 'error' | 'info' | 'warning'
  message: string
}>>([])

// Performance monitoring
const renderTime = ref(0)
const memoryUsage = ref(0)
const fps = ref(0)
const messageCount = ref(0)

// Intersection Observer for header animation
const headerRef = ref<HTMLElement | null>(null)
const { isVisible: isHeaderVisible } = useIntersectionObserver(
  headerRef,
  () => {},
  { threshold: 0.1, rootMargin: '50px' }
)

// Performance monitoring
let performanceObserver: PerformanceObserver | null = null
let frameCount = 0
let lastTime = performance.now()

const startPerformanceMonitoring = () => {
  if (!isDevelopment.value) return

  // Monitor render performance
  if ('PerformanceObserver' in window) {
    performanceObserver = new PerformanceObserver((list) => {
      const entries = list.getEntries()
      for (const entry of entries) {
        if (entry.entryType === 'measure') {
          renderTime.value = Math.round(entry.duration)
        }
      }
    })

    performanceObserver.observe({ entryTypes: ['measure', 'navigation'] })
  }

  // Monitor memory usage
  const updateMemoryUsage = () => {
    if ('memory' in performance) {
      const memory = (performance as any).memory
      memoryUsage.value = Math.round(memory.usedJSHeapSize / 1024 / 1024)
    }
  }

  // Monitor FPS
  const updateFPS = (currentTime: number) => {
    frameCount++
    
    if (currentTime >= lastTime + 1000) {
      fps.value = Math.round((frameCount * 1000) / (currentTime - lastTime))
      frameCount = 0
      lastTime = currentTime
      updateMemoryUsage()
    }
    
    requestAnimationFrame(updateFPS)
  }

  requestAnimationFrame(updateFPS)
}

// Toast notification system
const addToast = (type: 'success' | 'error' | 'info' | 'warning', message: string) => {
  const id = `toast-${Date.now()}-${Math.random()}`
  toasts.value.push({ id, type, message })
  
  // Auto-remove after 5 seconds
  setTimeout(() => {
    removeToast(id)
  }, 5000)
}

const removeToast = (id: string) => {
  const index = toasts.value.findIndex(toast => toast.id === id)
  if (index > -1) {
    toasts.value.splice(index, 1)
  }
}

// XState Inspector integration
const openInspector = () => {
  if (isDevelopment.value) {
    window.open('https://stately.ai/inspect', '_blank')
  }
}

// Event handlers
const handleConnectionStatus = (status: string) => {
  connectionStatus.value = status
  
  if (status === 'connected') {
    addToast('success', 'Connected to chat')
  } else if (status === 'disconnected') {
    addToast('error', 'Disconnected from chat')
  } else if (status === 'reconnecting') {
    addToast('warning', 'Reconnecting...')
  }
}

const handleRetry = () => {
  addToast('info', 'Retrying...')
  // Force component remount or specific retry logic
}

// Development tools
const enableDeveloperMode = () => {
  if (isDevelopment.value) {
    showInspector.value = true
    showPerformanceMonitor.value = true
    
    // Initialize XState Inspector
    try {
      initializeXStateInspector()
      addToast('success', 'Developer mode enabled')
    } catch (error) {
      addToast('error', 'Failed to enable developer mode')
    }
  }
}

// Keyboard shortcuts for development
const handleKeydown = (event: KeyboardEvent) => {
  if (!isDevelopment.value) return

  // Ctrl/Cmd + Shift + D = Developer mode
  if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key === 'D') {
    event.preventDefault()
    enableDeveloperMode()
  }

  // Ctrl/Cmd + Shift + I = Toggle inspector
  if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key === 'I') {
    event.preventDefault()
    showInspector.value = !showInspector.value
  }

  // Ctrl/Cmd + Shift + P = Toggle performance monitor
  if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key === 'P') {
    event.preventDefault()
    showPerformanceMonitor.value = !showPerformanceMonitor.value
  }
}

onMounted(() => {
  // Start performance monitoring
  startPerformanceMonitoring()
  
  // Add keyboard shortcuts
  document.addEventListener('keydown', handleKeydown)
  
  // Show development hint
  if (isDevelopment.value) {
    setTimeout(() => {
      addToast('info', 'Press Ctrl+Shift+D for developer tools')
    }, 2000)
  }
})

onUnmounted(() => {
  // Cleanup
  if (performanceObserver) {
    performanceObserver.disconnect()
  }
  
  document.removeEventListener('keydown', handleKeydown)
})
</script>

<style scoped>
/* Transition animations */
.slide-down-enter-active,
.slide-down-leave-active {
  transition: all 0.5s ease-out;
}

.slide-down-enter-from {
  transform: translateY(-100%);
  opacity: 0;
}

.slide-down-leave-to {
  transform: translateY(-100%);
  opacity: 0;
}

/* Toast animations */
.toast-enter-active,
.toast-leave-active {
  transition: all 0.3s ease-out;
}

.toast-enter-from {
  transform: translateX(-100%);
  opacity: 0;
}

.toast-leave-to {
  transform: translateX(-100%);
  opacity: 0;
}

.toast-move {
  transition: transform 0.3s ease;
}
</style>
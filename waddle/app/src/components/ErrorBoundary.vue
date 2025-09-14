<template>
  <div v-if="hasError" class="min-h-screen flex items-center justify-center bg-gradient-to-br from-black to-gray-900">
    <div class="max-w-md mx-auto text-center p-8">
      <div class="bg-red-500/10 border border-red-500/20 rounded-2xl p-6 backdrop-blur-sm">
        <!-- Error Icon -->
        <div class="w-16 h-16 mx-auto mb-4 bg-red-500/20 rounded-full flex items-center justify-center">
          <svg class="w-8 h-8 text-red-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-2.5L13.732 4c-.77-.833-1.964-.833-2.732 0L4.082 16.5c-.77.833.192 2.5 1.732 2.5z"></path>
          </svg>
        </div>

        <h2 class="text-xl font-bold text-white mb-2">Something went wrong</h2>
        <p class="text-white/70 text-sm mb-4">{{ errorMessage }}</p>
        
        <!-- Error Details (in development) -->
        <details v-if="isDevelopment && errorDetails" class="mb-4 text-left">
          <summary class="text-xs text-white/50 cursor-pointer hover:text-white/70">Error Details</summary>
          <pre class="text-xs text-red-400 mt-2 p-2 bg-black/20 rounded overflow-auto">{{ errorDetails }}</pre>
        </details>

        <!-- Action Buttons -->
        <div class="flex flex-col sm:flex-row gap-3">
          <button
            @click="retry"
            class="flex-1 px-4 py-2 bg-accent-primary hover:bg-accent-primary-dark text-white font-medium rounded-lg transition-colors"
          >
            Try Again
          </button>
          <button
            @click="goHome"
            class="flex-1 px-4 py-2 bg-white/10 hover:bg-white/20 text-white font-medium rounded-lg transition-colors border border-white/20"
          >
            Go Home
          </button>
        </div>

        <!-- Report Bug Link -->
        <button
          @click="reportBug"
          class="mt-4 text-xs text-white/50 hover:text-white/70 transition-colors"
        >
          Report this issue
        </button>
      </div>
    </div>
  </div>
  
  <slot v-else />
</template>

<script setup lang="ts">
import { ref, onErrorCaptured, nextTick } from 'vue'

interface Props {
  fallback?: string
  onError?: (error: Error, instance: any, info: string) => void
  onRetry?: () => void
}

const props = defineProps<Props>()

const hasError = ref(false)
const errorMessage = ref('An unexpected error occurred.')
const errorDetails = ref('')
const isDevelopment = ref(import.meta.env.DEV)

// Capture errors from child components
onErrorCaptured((error: Error, instance, info) => {
  console.error('Error caught by ErrorBoundary:', error, info)
  
  hasError.value = true
  errorMessage.value = error.message || 'An unexpected error occurred.'
  
  if (isDevelopment.value) {
    errorDetails.value = `${error.name}: ${error.message}\n\nStack trace:\n${error.stack}\n\nComponent info: ${info}`
  }
  
  // Call custom error handler if provided
  if (props.onError) {
    props.onError(error, instance, info)
  }
  
  // Prevent the error from propagating further
  return false
})

const retry = async () => {
  hasError.value = false
  errorMessage.value = ''
  errorDetails.value = ''
  
  // Call custom retry handler if provided
  if (props.onRetry) {
    props.onRetry()
  }
  
  // Force re-render
  await nextTick()
}

const goHome = () => {
  window.location.href = '/'
}

const reportBug = () => {
  const subject = encodeURIComponent('Bug Report: ' + errorMessage.value)
  const body = encodeURIComponent(`Error: ${errorMessage.value}\n\nDetails: ${errorDetails.value}\n\nURL: ${window.location.href}\n\nUser Agent: ${navigator.userAgent}`)
  
  window.open(`mailto:support@example.com?subject=${subject}&body=${body}`)
}
</script>
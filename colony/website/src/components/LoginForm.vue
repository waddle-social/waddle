<template>
  <div class="min-h-screen bg-gradient-to-br from-gray-900 via-blue-900 to-indigo-900 flex items-center justify-center p-4 bg-grid-pattern">
    <div class="w-full max-w-md animate-slide-up">
      <!-- Main Card -->
      <div class="glass-dark backdrop-blur-lg shadow-2xl border border-gray-700 rounded-xl bg-gray-800/90">
        <div class="p-6 pb-4">
          <div class="flex items-center justify-center mb-4">
            <div class="p-3 bg-gradient-to-br from-blue-500 to-indigo-600 rounded-full animate-pulse-glow">
              <svg class="w-8 h-8 text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 12a9 9 0 01-9 9m9-9a9 9 0 00-9-9m9 9H3m9 9a9 9 0 01-9-9m9 9c1.657 0 3-4.03 3-9s-1.343-9-3-9m0 18c-1.657 0-3-4.03-3-9s1.343-9 3-9m-9 9a9 9 0 019-9" />
              </svg>
            </div>
          </div>
          <h1 class="text-2xl font-bold text-center bg-gradient-to-r from-blue-400 to-indigo-400 bg-clip-text text-transparent">
            Colony
          </h1>
          <p class="text-center mt-2 text-gray-300">
            Connect with your decentralized AT Protocol identity
          </p>
        </div>

        <div class="px-6 pb-6">
          <form @submit.prevent="handleSubmit" class="space-y-4">
            <!-- Handle Input -->
            <div class="space-y-2">
              <label for="handle" class="text-sm font-medium text-gray-200">
                AT Protocol Handle
              </label>
              <div class="relative">
                <span class="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400">@</span>
                <input
                  id="handle"
                  v-model="handle"
                  type="text"
                  placeholder="yourhandle.bsky.social"
                  class="pl-8 w-full h-11 px-3 py-2 bg-gray-900 border border-gray-600 text-white rounded-lg transition-all-smooth focus:ring-2 focus:ring-blue-500 focus:border-transparent placeholder-gray-500"
                  :disabled="loading"
                  @input="clearError"
                />
              </div>
              <p class="text-xs text-gray-400">
                Enter your AT Protocol handle (e.g., alice.bsky.social)
              </p>
            </div>

            <!-- Error Alert -->
            <div v-if="error" class="bg-red-900/30 border border-red-800 text-red-400 px-4 py-3 rounded-lg animate-slide-up">
              <p class="text-sm">{{ error }}</p>
            </div>

            <!-- Submit Button -->
            <button
              type="submit"
              class="w-full h-11 px-4 py-2 bg-gradient-to-r from-blue-600 to-indigo-600 hover:from-blue-700 hover:to-indigo-700 text-white font-medium rounded-lg transition-all-smooth disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center"
              :disabled="!handle || loading"
            >
              <template v-if="loading">
                <svg class="animate-spin -ml-1 mr-3 h-5 w-5 text-white" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                  <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                  <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                </svg>
                Connecting...
              </template>
              <template v-else>
                <svg class="w-5 h-5 mr-2" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13 10V3L4 14h7v7l9-11h-7z" />
                </svg>
                Connect with AT Protocol
              </template>
            </button>

            <!-- Divider -->
            <div class="relative my-6">
              <div class="absolute inset-0 flex items-center">
                <div class="w-full border-t border-gray-200"></div>
              </div>
              <div class="relative flex justify-center text-xs uppercase">
                <span class="bg-gray-800 px-2 text-gray-400">Secure Authentication</span>
              </div>
            </div>

            <!-- Info Section -->
            <div class="space-y-3">
              <div class="flex items-center space-x-2 text-sm text-gray-300">
                <svg class="w-4 h-4 text-green-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
                </svg>
                <span>OAuth 2.0 with DPoP protection</span>
              </div>
              <div class="flex items-center space-x-2 text-sm text-gray-300">
                <svg class="w-4 h-4 text-green-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z" />
                </svg>
                <span>End-to-end encrypted</span>
              </div>
              <div class="flex items-center space-x-2 text-sm text-gray-300">
                <svg class="w-4 h-4 text-green-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                </svg>
                <span>No passwords required</span>
              </div>
            </div>
          </form>
        </div>

        <div class="px-6 pb-6 pt-2">
          <p class="text-xs text-center text-gray-400">
            By connecting, you authorize this app to use your AT Protocol identity
          </p>
          <p class="text-xs text-center mt-2">
            Don't have an account?
            <a href="https://bsky.app" target="_blank" class="text-blue-400 hover:text-blue-300 font-medium">
              Create one here
            </a>
          </p>
        </div>
      </div>

      <!-- Footer Badges -->
      <div class="mt-6 flex justify-center space-x-4">
        <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-gray-800/80 backdrop-blur text-gray-300 border border-gray-700">
          <svg class="w-3 h-3 mr-1" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 18h.01M8 21h8a2 2 0 002-2V5a2 2 0 00-2-2H8a2 2 0 00-2 2v14a2 2 0 002 2z" />
          </svg>
          Edge Computing
        </span>
        <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-gray-800/80 backdrop-blur text-gray-300 border border-gray-700">
          <svg class="w-3 h-3 mr-1" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 20l4-16m4 4l4 4-4 4M6 16l-4-4 4-4" />
          </svg>
          Open Source
        </span>
        <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-gray-800/80 backdrop-blur text-gray-300 border border-gray-700">
          <svg class="w-3 h-3 mr-1" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z" />
          </svg>
          Privacy First
        </span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue';

const props = defineProps<{
  redirectTo?: string;
}>();

const handle = ref('');
const loading = ref(false);
const error = ref('');

const clearError = () => {
  error.value = '';
};

const handleSubmit = async () => {
  if (!handle.value) return;

  loading.value = true;
  error.value = '';

  try {
    let processedHandle = handle.value.trim();

    // Clean up handle format
    if (!processedHandle.startsWith('@')) {
      processedHandle = '@' + processedHandle;
    }
    if (!processedHandle.includes('.')) {
      processedHandle = processedHandle + '.bsky.social';
    }

    // Validate handle format
    const handleRegex = /^@?[\w.-]+(\.[a-z]+)?$/i;
    if (!handleRegex.test(processedHandle)) {
      throw new Error('Please enter a valid AT Protocol handle');
    }

    // Use the Better Auth ATProto signin endpoint
    const payload: Record<string, string> = {
      handle: processedHandle.substring(1),
    };
    if (props.redirectTo) {
      payload.redirectTo = props.redirectTo;
    }

    const response = await fetch('/api/auth/atproto/signin', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(payload),
    });

    const data = await response.json();

    if (!response.ok) {
      throw new Error(data.error || 'Failed to initiate authentication');
    }

    // Redirect to AT Protocol OAuth
    if (data.authUrl) {
      window.location.href = data.authUrl;
    } else {
      throw new Error('No authorization URL received');
    }
  } catch (err: any) {
    console.error('Authentication error:', err);
    error.value = err.message || 'Failed to connect with AT Protocol';
    loading.value = false;
  }
};

// Check for error in URL on mount (client-side only)
if (typeof window !== 'undefined') {
  const urlParams = new URLSearchParams(window.location.search);
  const urlError = urlParams.get('error');
  if (urlError === 'auth_failed') {
    error.value = 'Authentication failed. Please try again.';
  }
}
</script>

<style scoped>
/* Import the custom styles from tailwind.css */
@import '../tailwind.css';
</style>

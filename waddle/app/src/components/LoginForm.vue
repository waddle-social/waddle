<template>
  <!-- Loading state while XState initializes -->
  <div v-if="!snapshot" class="min-h-screen flex items-center justify-center bg-gradient-to-br from-black to-gray-900">
    <div class="text-white text-xl">Loading...</div>
  </div>
  
  <!-- Main component -->
  <div v-else class="min-h-screen flex items-center justify-center relative overflow-hidden">
    <!-- Enhanced floating orbs -->
    <div class="floating-orb w-80 h-80 top-1/4 left-1/4 animate-float"></div>
    <div class="floating-orb w-96 h-96 bottom-1/4 right-1/4 animate-float-delayed"></div>
    <div class="floating-orb w-64 h-64 top-3/4 right-1/3 animate-float"></div>
    
    <!-- Main login card -->
    <div class="relative z-10 w-full max-w-md p-8 m-4">
      <!-- Enhanced Glassmorphism card -->
      <div class="glass backdrop-blur-xl rounded-4xl p-8 shadow-glass-xl border border-glass-border-strong shimmer-effect relative overflow-hidden">
        <!-- Subtle gradient overlay -->
        <div class="absolute inset-0 bg-gradient-to-br from-white/5 via-transparent to-transparent pointer-events-none rounded-4xl"></div>
        <div class="relative z-10">
          <!-- Logo/Title -->
          <div class="text-center mb-8">
            <h1 class="text-5xl font-bold text-white mb-3 bg-gradient-to-r from-primary-300 via-accent-primary to-primary-400 bg-clip-text text-transparent animate-pulse-subtle">
              Waddle
            </h1>
            <p class="text-white/80 mt-2 text-base font-medium">Enter the future of community</p>
          </div>

          <!-- Login Form -->
          <form @submit.prevent="handleLogin" class="space-y-6">
            <!-- Username Input -->
            <div class="space-y-3">
              <label for="username" class="block text-sm font-semibold text-white/90 tracking-wide">
                Username
              </label>
              <div class="relative group">
                <input
                  id="username"
                  v-model="username"
                  type="text"
                  required
                  :disabled="snapshot?.matches?.('authenticating')"
                  class="w-full px-5 py-4 glass-surface border border-glass-border rounded-2xl text-white placeholder-white/60 backdrop-blur-md focus:outline-none focus:ring-2 focus:ring-accent-primary/60 focus:border-accent-primary/30 hover:border-glass-border-strong transition-all duration-300 disabled:opacity-50 text-lg"
                  placeholder="Enter your username"
                />
                <div class="absolute inset-0 rounded-2xl bg-gradient-to-r from-accent-primary/0 via-accent-primary/0 to-accent-primary/0 group-hover:from-accent-primary/5 group-hover:via-accent-primary/10 group-hover:to-accent-primary/5 transition-all duration-300 pointer-events-none"></div>
              </div>
            </div>

            <!-- Error Message -->
            <div v-if="snapshot?.context?.error" class="glass-surface border border-red-400/30 rounded-2xl p-4 backdrop-blur-md">
              <div class="flex items-center space-x-3">
                <div class="w-5 h-5 rounded-full bg-red-400/20 flex items-center justify-center">
                  <div class="w-2 h-2 rounded-full bg-red-400"></div>
                </div>
                <span class="text-red-300 text-sm font-medium">{{ snapshot.context.error }}</span>
              </div>
            </div>

            <!-- Login Button -->
            <button
              type="submit"
              :disabled="snapshot?.matches?.('authenticating') || !username.trim()"
              class="w-full py-4 px-6 bg-gradient-to-r from-accent-primary to-primary-600 hover:from-accent-primary-dark hover:to-primary-700 text-white font-bold rounded-2xl transition-all duration-300 transform hover:scale-[1.02] hover:shadow-glow active:scale-[0.98] disabled:opacity-50 disabled:transform-none disabled:hover:scale-100 focus:outline-none focus:ring-2 focus:ring-accent-primary/50 shadow-glass-lg text-lg tracking-wide relative overflow-hidden group"
            >
              <!-- Button shine effect -->
              <div class="absolute inset-0 bg-gradient-to-r from-transparent via-white/20 to-transparent opacity-0 group-hover:opacity-100 group-hover:animate-shimmer transition-opacity duration-300"></div>
              
              <span v-if="snapshot?.matches?.('authenticating')" class="flex items-center justify-center relative z-10">
                <svg class="animate-spin -ml-1 mr-3 h-5 w-5 text-white" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                  <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                  <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                </svg>
                Entering...
              </span>
              <span v-else class="relative z-10">Enter Chat</span>
            </button>
          </form>

          <!-- Footer -->
          <div class="mt-8 text-center">
            <p class="text-white/60 text-sm font-medium tracking-wide">
              🌟 Join the community • Real-time conversations
            </p>
          </div>
        </div>
      </div>
    </div>

    <!-- Subtle grid overlay -->
    <div class="absolute inset-0 opacity-20 bg-[url('/grid-pattern.svg')] pointer-events-none"></div>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { useMachine } from '@xstate/vue'
import { authMachine } from '../machines/authMachine'

const { snapshot, send } = useMachine(authMachine)
const username = ref('')

const handleLogin = async () => {
  if (!username.value.trim()) return
  
  send({ type: 'LOGIN', username: username.value.trim() })
  
  try {
    // Simulate authentication (replace with real auth logic)
    await new Promise(resolve => setTimeout(resolve, 1500))
    
    // In a real app, validate credentials here
    if (username.value.trim().length < 2) {
      send({ type: 'LOGIN_ERROR', error: 'Username must be at least 2 characters' })
    } else {
      send({ type: 'LOGIN_SUCCESS' })
      
      // Emit custom DOM event for Astro to handle
      const event = new CustomEvent('login-success', {
        detail: { username: username.value.trim() },
        bubbles: true
      })
      document.dispatchEvent(event)
    }
  } catch (error) {
    send({ type: 'LOGIN_ERROR', error: 'Login failed. Please try again.' })
  }
}
</script>

<style scoped>
/* Enhanced Responsive Design for Login Form */
@media (max-width: 768px) {
  .floating-orb {
    display: none;
  }
  
  .glass {
    margin: 1rem;
    padding: 2rem 1.5rem;
  }
  
  .text-5xl {
    font-size: 2.5rem;
  }
  
  .text-lg {
    font-size: 1rem;
  }
  
  .py-4 {
    padding-top: 0.75rem;
    padding-bottom: 0.75rem;
  }
}

@media (max-width: 480px) {
  .glass {
    margin: 0.5rem;
    padding: 1.5rem 1rem;
  }
  
  .text-5xl {
    font-size: 2rem;
  }
  
  .text-base {
    font-size: 0.875rem;
  }
}

/* Enhanced accessibility */
@media (prefers-reduced-motion: reduce) {
  .animate-pulse-subtle,
  .animate-float,
  .animate-float-delayed,
  .shimmer-effect::before {
    animation: none;
  }
}

@media (prefers-contrast: high) {
  .glass {
    background: rgba(255, 255, 255, 0.15);
    border: 2px solid rgba(255, 255, 255, 0.3);
  }
  
  .glass-surface {
    background: rgba(255, 255, 255, 0.1);
    border: 2px solid rgba(255, 255, 255, 0.2);
  }
}
</style>
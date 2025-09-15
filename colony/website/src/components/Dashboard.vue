<template>
  <div class="min-h-screen bg-gradient-to-br from-gray-900 to-gray-800">
    <!-- Navigation Header -->
    <header class="bg-gray-900 shadow-sm border-b border-gray-700">
      <div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
        <div class="flex justify-between items-center h-16">
          <div class="flex items-center">
            <div class="flex-shrink-0 flex items-center">
              <div class="p-2 bg-gradient-to-br from-blue-500 to-indigo-600 rounded-lg">
                <svg class="w-6 h-6 text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 12a9 9 0 01-9 9m9-9a9 9 0 00-9-9m9 9H3m9 9a9 9 0 01-9-9m9 9c1.657 0 3-4.03 3-9s-1.343-9-3-9m0 18c-1.657 0-3-4.03-3-9s1.343-9 3-9m-9 9a9 9 0 019-9" />
                </svg>
              </div>
              <span class="ml-3 text-xl font-bold bg-gradient-to-r from-blue-400 to-indigo-400 bg-clip-text text-transparent">
                Colony
              </span>
            </div>
          </div>

          <div class="flex items-center space-x-4">
            <div class="relative">
              <button @click="showDropdown = !showDropdown" class="flex items-center space-x-2 text-sm font-medium text-gray-300 hover:text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 rounded-lg p-2">
                <div class="h-8 w-8 rounded-full bg-gradient-to-br from-blue-500 to-indigo-600 flex items-center justify-center text-white font-semibold">
                  {{ (user?.name || user?.handle || 'U').charAt(0).toUpperCase() }}
                </div>
                <span class="hidden sm:block text-gray-200">{{ user?.name || user?.handle }}</span>
                <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7" />
                </svg>
              </button>

              <div v-if="showDropdown" class="absolute right-0 mt-2 w-48 bg-gray-800 border border-gray-700 rounded-lg shadow-lg py-1 z-50">
                <button @click="handleLogout" class="block w-full text-left px-4 py-2 text-sm text-red-400 hover:bg-gray-700">
                  <svg class="inline w-4 h-4 mr-2" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M17 16l4-4m0 0l-4-4m4 4H7m6 4v1a3 3 0 01-3 3H6a3 3 0 01-3-3V7a3 3 0 013-3h4a3 3 0 013 3v1" />
                  </svg>
                  Logout
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </header>

    <!-- Main Content -->
    <main class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 py-8">
      <!-- Welcome Section -->
      <div class="mb-8 animate-slide-up">
        <div class="bg-gradient-to-r from-blue-600 to-indigo-600 text-white rounded-xl p-8">
          <div class="flex items-center space-x-6">
            <div class="h-20 w-20 rounded-full bg-white/20 flex items-center justify-center text-white text-2xl font-bold border-4 border-white/20">
              {{ (user?.name || user?.handle || 'U').charAt(0).toUpperCase() }}
            </div>
            <div>
              <h1 class="text-3xl font-bold mb-2">
                Hello, {{ user?.name || user?.handle || 'User' }}!
              </h1>
              <p class="text-blue-100">
                Welcome back to Colony - Your AT Protocol identity hub
              </p>
            </div>
          </div>
        </div>
      </div>

      <!-- Stats Grid -->
      <div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4 mb-8">
        <div v-for="stat in stats" :key="stat.title" class="bg-gray-800 border border-gray-700 rounded-lg shadow hover:shadow-lg transition-all-smooth animate-slide-up p-6">
          <div class="flex items-center justify-between">
            <div>
              <p class="text-sm font-medium text-gray-400">{{ stat.title }}</p>
              <p class="text-2xl font-bold mt-1 text-white">{{ stat.value }}</p>
            </div>
            <div class="p-3 rounded-lg" :class="`bg-${stat.color}-100`">
              <svg class="w-6 h-6" :class="`text-${stat.color}-600`" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" :d="stat.icon" />
              </svg>
            </div>
          </div>
        </div>
      </div>

      <!-- Main Content Grid -->
      <div class="grid grid-cols-1 lg:grid-cols-3 gap-6">
        <!-- Identity Card -->
        <div class="lg:col-span-2">
          <div class="bg-gray-800 border border-gray-700 rounded-lg shadow animate-slide-up">
            <div class="p-6 border-b border-gray-700">
              <h2 class="text-lg font-semibold text-white flex items-center">
                <svg class="w-5 h-5 mr-2 text-blue-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H5a2 2 0 00-2 2v9a2 2 0 002 2h14a2 2 0 002-2V8a2 2 0 00-2-2h-5m-4 0V5a2 2 0 114 0v1m-4 0a2 2 0 104 0m-5 8a2 2 0 100-4 2 2 0 000 4zm0 0c1.306 0 2.417.835 2.83 2M9 14a3.001 3.001 0 00-2.83 2M15 11h3m-3 4h2" />
                </svg>
                AT Protocol Identity
              </h2>
              <p class="text-sm text-gray-400 mt-1">Your decentralized identity information</p>
            </div>
            <div class="p-6">
              <div class="space-y-4">
                <div class="flex items-center justify-between p-3 bg-gray-900 rounded-lg">
                  <div>
                    <p class="text-sm font-medium text-gray-400">Handle</p>
                    <p class="font-mono text-white">@{{ user?.handle }}</p>
                  </div>
                  <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-green-100 text-green-800">
                    <svg class="w-3 h-3 mr-1" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7" />
                    </svg>
                    Verified
                  </span>
                </div>

                <div v-if="user?.did" class="flex items-center justify-between p-3 bg-gray-50 rounded-lg">
                  <div class="flex-1 mr-4">
                    <p class="text-sm font-medium text-gray-400">DID</p>
                    <p class="font-mono text-xs break-all text-gray-300">{{ user.did }}</p>
                  </div>
                  <button @click="copyDID" class="p-2 hover:bg-gray-700 rounded-lg transition-colors text-gray-400">
                    <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 16H6a2 2 0 01-2-2V6a2 2 0 012-2h8a2 2 0 012 2v2m-6 12h8a2 2 0 002-2v-8a2 2 0 00-2-2h-8a2 2 0 00-2 2v8a2 2 0 002 2z" />
                    </svg>
                  </button>
                </div>

                <div class="p-3 bg-gray-50 rounded-lg">
                  <p class="text-sm font-medium text-gray-600 mb-2">Security</p>
                  <div class="flex items-center space-x-2">
                    <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-blue-100 text-blue-800">
                      OAuth 2.0
                    </span>
                    <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-purple-100 text-purple-800">
                      DPoP Protected
                    </span>
                    <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-green-100 text-green-800">
                      PKCE
                    </span>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- Quick Actions -->
        <div class="space-y-4">
          <div class="bg-gray-800 border border-gray-700 rounded-lg shadow animate-slide-up">
            <div class="p-6 border-b border-gray-700">
              <h3 class="text-lg font-semibold text-white flex items-center">
                <svg class="w-5 h-5 mr-2 text-blue-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13 10V3L4 14h7v7l9-11h-7z" />
                </svg>
                Quick Actions
              </h3>
            </div>
            <div class="p-6 space-y-2">
              <button @click="handleLogout" class="w-full px-4 py-2 bg-red-600 text-white rounded-lg hover:bg-red-700 transition-colors flex items-center justify-center">
                <svg class="w-4 h-4 mr-2" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M17 16l4-4m0 0l-4-4m4 4H7m6 4v1a3 3 0 01-3 3H6a3 3 0 01-3-3V7a3 3 0 013-3h4a3 3 0 013 3v1" />
                </svg>
                Logout
              </button>
            </div>
          </div>

          <!-- About Colony -->
          <div class="bg-gray-800 border border-gray-700 rounded-lg shadow animate-slide-up">
            <div class="p-6 border-b border-gray-700">
              <h3 class="text-lg font-semibold text-white flex items-center">
                <svg class="w-5 h-5 mr-2 text-blue-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                </svg>
                About Colony
              </h3>
            </div>
            <div class="p-6">
              <p class="text-sm text-gray-400 mb-4">
                Colony is a modern authentication service built on the AT Protocol,
                providing decentralized identity management with privacy and security at its core.
              </p>
              <div class="flex flex-wrap gap-2">
                <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-gray-100 text-gray-800">
                  Open Source
                </span>
                <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-gray-100 text-gray-800">
                  Edge Computing
                </span>
                <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-gray-100 text-gray-800">
                  Privacy First
                </span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </main>

    <!-- Custom Toast notification -->
    <div v-if="showToast" class="fixed bottom-4 right-4 z-50 animate-slide-up">
      <div class="bg-gray-800 rounded-lg shadow-lg p-4 flex items-center space-x-2 border border-gray-700">
        <svg class="w-5 h-5 text-green-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7" />
        </svg>
        <span class="text-sm font-medium text-white">{{ toastMessage }}</span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue';

const props = defineProps<{
  user: {
    name?: string;
    handle: string;
    did?: string;
    image?: string;
  };
}>();

const showDropdown = ref(false);
const showToast = ref(false);
const toastMessage = ref('');

const stats = [
  {
    title: 'Active Sessions',
    value: '1',
    color: 'green',
    icon: 'M15 7a2 2 0 012 2m4 0a6 6 0 01-7.743 5.743L11 17H9v2H7v2H4a1 1 0 01-1-1v-2.586a1 1 0 01.293-.707l5.964-5.964A6 6 0 1121 9z'
  },
  {
    title: 'Connected Apps',
    value: '3',
    color: 'blue',
    icon: 'M4 6a2 2 0 012-2h2a2 2 0 012 2v2a2 2 0 01-2 2H6a2 2 0 01-2-2V6zM14 6a2 2 0 012-2h2a2 2 0 012 2v2a2 2 0 01-2 2h-2a2 2 0 01-2-2V6zM4 16a2 2 0 012-2h2a2 2 0 012 2v2a2 2 0 01-2 2H6a2 2 0 01-2-2v-2zM14 16a2 2 0 012-2h2a2 2 0 012 2v2a2 2 0 01-2 2h-2a2 2 0 01-2-2v-2z'
  },
  {
    title: 'Security Score',
    value: '100%',
    color: 'purple',
    icon: 'M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z'
  },
  {
    title: 'Last Login',
    value: 'Today',
    color: 'yellow',
    icon: 'M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z'
  },
];

const handleLogout = async () => {
  const form = document.createElement('form');
  form.method = 'POST';
  form.action = '/api/auth/logout';
  document.body.appendChild(form);
  form.submit();
};

const copyDID = () => {
  if (props.user?.did) {
    navigator.clipboard.writeText(props.user.did);
    toastMessage.value = 'DID copied to clipboard!';
    showToast.value = true;
    setTimeout(() => {
      showToast.value = false;
    }, 3000);
  }
};

// Close dropdown when clicking outside (client-side only)
if (typeof window !== 'undefined') {
  window.addEventListener('click', (e) => {
    if (!(e.target as HTMLElement).closest('.relative')) {
      showDropdown.value = false;
    }
  });
}
</script>

<style scoped>
@import '../tailwind.css';
</style>
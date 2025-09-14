<template>
  <aside class="w-64 bg-white dark:bg-neutral-900 border-r border-neutral-200 dark:border-neutral-800 min-h-screen">
    <!-- User Profile Section -->
    <div class="p-6 border-b border-neutral-200 dark:border-neutral-800">
      <div class="flex items-center space-x-3">
        <div class="w-10 h-10 rounded-full bg-primary-500 flex items-center justify-center text-white font-medium">
          {{ userInitials }}
        </div>
        <div class="flex-1 min-w-0">
          <p class="text-sm font-medium text-neutral-900 dark:text-white truncate">
            {{ user?.name || 'Guest User' }}
          </p>
          <p class="text-xs text-neutral-600 dark:text-neutral-400 truncate">
            {{ user?.handle || '@guest' }}
          </p>
        </div>
      </div>
    </div>

    <!-- Navigation -->
    <nav class="p-4 space-y-1">
      <div v-for="section in navSections" :key="section.title" class="mb-6">
        <h3 v-if="section.title" class="px-3 mb-2 text-xs font-semibold text-neutral-500 dark:text-neutral-400 uppercase tracking-wider">
          {{ section.title }}
        </h3>
        
        <div class="space-y-1">
          <a
            v-for="item in section.items"
            :key="item.href"
            :href="item.href"
            :class="[
              'group flex items-center px-3 py-2 text-sm font-medium rounded-lg transition-colors',
              isActive(item.href)
                ? 'bg-primary-50 text-primary-700 dark:bg-primary-950 dark:text-primary-300'
                : 'text-neutral-700 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800'
            ]"
          >
            <component
              :is="item.icon"
              :class="[
                'mr-3 h-5 w-5 transition-colors',
                isActive(item.href)
                  ? 'text-primary-600 dark:text-primary-400'
                  : 'text-neutral-400 group-hover:text-neutral-600 dark:group-hover:text-neutral-300'
              ]"
            />
            {{ item.label }}
            
            <!-- Badge for notifications -->
            <span
              v-if="item.badge"
              class="ml-auto inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium bg-primary-100 text-primary-800 dark:bg-primary-900 dark:text-primary-200"
            >
              {{ item.badge }}
            </span>
          </a>
        </div>
      </div>
    </nav>

    <!-- Bottom Actions -->
    <div class="absolute bottom-0 left-0 right-0 p-4 border-t border-neutral-200 dark:border-neutral-800">
      <button
        @click="$emit('toggle-collapsed')"
        class="w-full flex items-center justify-center px-3 py-2 text-sm font-medium text-neutral-700 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800 rounded-lg transition-colors"
      >
        <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M11 19l-7-7 7-7m8 14l-7-7 7-7" />
        </svg>
        <span class="ml-2">Collapse</span>
      </button>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { computed, h } from 'vue'
import { 
  HomeIcon,
  CalendarIcon,
  ClockIcon,
  LinkIcon,
  UserGroupIcon,
  CogIcon,
  ChartBarIcon,
  BellIcon
} from './icons'

interface User {
  name: string
  handle: string
  did: string
}

interface NavItem {
  label: string
  href: string
  icon: any
  badge?: number | string
}

interface NavSection {
  title?: string
  items: NavItem[]
}

const props = defineProps<{
  user?: User
  currentPath?: string
  pendingBookings?: number
}>()

const emit = defineEmits<{
  'toggle-collapsed': []
}>()

const userInitials = computed(() => {
  if (!props.user) return 'G'
  return props.user.name
    .split(' ')
    .map(n => n[0])
    .join('')
    .toUpperCase()
    .slice(0, 2)
})

const navSections = computed<NavSection[]>(() => [
  {
    items: [
      { label: 'Dashboard', href: '/dashboard', icon: HomeIcon },
      { label: 'Bookings', href: '/dashboard/bookings', icon: CalendarIcon, badge: props.pendingBookings },
    ]
  },
  {
    title: 'Manage',
    items: [
      { label: 'My Offers', href: '/dashboard/offers', icon: ClockIcon },
      { label: 'Calendar Connections', href: '/dashboard/connectors', icon: LinkIcon },
      { label: 'Followers', href: '/dashboard/followers', icon: UserGroupIcon },
    ]
  },
  {
    title: 'Settings',
    items: [
      { label: 'Preferences', href: '/dashboard/settings', icon: CogIcon },
      { label: 'Notifications', href: '/dashboard/notifications', icon: BellIcon },
      { label: 'Analytics', href: '/dashboard/analytics', icon: ChartBarIcon },
    ]
  }
])

const isActive = (href: string): boolean => {
  if (!props.currentPath) return false
  return props.currentPath === href || props.currentPath.startsWith(href + '/')
}
</script>
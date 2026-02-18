<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { useRouter, useRoute } from 'vue-router';
import { storeToRefs } from 'pinia';

import { useConversations } from '../composables/useConversations';
import { useRuntimeStore } from '../stores/runtime';
import { useAuthStore } from '../stores/auth';
import { useRoomsStore } from '../stores/rooms';

const router = useRouter();
const route = useRoute();
const runtimeStore = useRuntimeStore();
const authStore = useAuthStore();
const roomsStore = useRoomsStore();
const { connectionStatus } = storeToRefs(runtimeStore);

const { conversations } = useConversations();
const { joinedRooms } = storeToRefs(roomsStore);

const directJid = ref('');

function getInitials(name: string): string {
  const parts = name.split(/[@.\s]+/).filter(Boolean);
  if (parts.length >= 2) {
    const first = parts[0]?.[0] ?? '';
    const second = parts[1]?.[0] ?? '';
    return `${first}${second}`.toUpperCase();
  }
  return name.slice(0, 2).toUpperCase();
}

function getAvatarColor(jid: string): string {
  const colors = [
    '#5865f2', '#57f287', '#fee75c', '#eb459e',
    '#ed4245', '#3ba55c', '#faa61a', '#5865f2',
  ];
  let hash = 0;
  for (const ch of jid) {
    hash = ch.charCodeAt(0) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length] ?? colors[0] ?? '#5865f2';
}

function isActiveChat(jid: string): boolean {
  if (route.name !== 'chat') return false;
  try {
    return decodeURIComponent(String(route.params.jid ?? '')) === jid;
  } catch {
    return false;
  }
}

function openChat(jid: string): void {
  void router.push(`/chat/${encodeURIComponent(jid)}`);
}

function openDirectMessage(): void {
  const jid = directJid.value.trim();
  if (!jid) return;
  directJid.value = '';
  openChat(jid);
}

function roomLocalpart(jid: string): string {
  return jid.split('@')[0] || jid;
}

function formatTime(value: string | null): string {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return '';
  const now = new Date();
  if (date.toDateString() === now.toDateString()) {
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
  return date.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

const statusDotClass = computed(() => {
  switch (connectionStatus.value) {
    case 'connected': return 'bg-success';
    case 'reconnecting': return 'bg-warning';
    case 'offline': return 'bg-danger';
    default: return 'bg-muted';
  }
});

const joinedRoomList = computed(() => Array.from(joinedRooms.value));

async function handleLogout(): Promise<void> {
  await authStore.logout();
  roomsStore.reset();
  void router.replace('/login');
}

// Start room discovery when sidebar mounts (user is authenticated)
onMounted(() => {
  roomsStore.startListening();
});

onUnmounted(() => {
  roomsStore.stopListening();
});
</script>

<template>
  <aside class="flex h-full w-60 flex-shrink-0 flex-col bg-sidebar">
    <!-- Workspace header -->
    <div class="flex h-12 items-center border-b border-border px-4 shadow-sm">
      <h1 class="truncate text-base font-semibold text-foreground">Waddle</h1>
    </div>

    <!-- Nav links -->
    <div class="flex flex-col gap-0.5 px-2 py-2">
      <RouterLink
        to="/roster"
        class="flex items-center gap-2 rounded px-2 py-1.5 text-sm text-muted transition-colors hover:bg-hover hover:text-foreground"
        active-class="!bg-active !text-foreground"
      >
        <svg class="h-4 w-4 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M17 20h5v-2a3 3 0 00-5.356-1.857M17 20H7m10 0v-2c0-.656-.126-1.283-.356-1.857M7 20H2v-2a3 3 0 015.356-1.857M7 20v-2c0-.656.126-1.283.356-1.857m0 0a5.002 5.002 0 019.288 0M15 7a3 3 0 11-6 0 3 3 0 016 0z" />
        </svg>
        Contacts
      </RouterLink>
      <RouterLink
        to="/rooms"
        class="flex items-center gap-2 rounded px-2 py-1.5 text-sm text-muted transition-colors hover:bg-hover hover:text-foreground"
        active-class="!bg-active !text-foreground"
      >
        <svg class="h-4 w-4 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M7 20l4-16m2 16l4-16M6 9h14M4 15h14" />
        </svg>
        Rooms
      </RouterLink>
      <RouterLink
        to="/settings"
        class="flex items-center gap-2 rounded px-2 py-1.5 text-sm text-muted transition-colors hover:bg-hover hover:text-foreground"
        active-class="!bg-active !text-foreground"
      >
        <svg class="h-4 w-4 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.066 2.573c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.573 1.066c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.066-2.573c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" />
          <path stroke-linecap="round" stroke-linejoin="round" d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
        </svg>
        Settings
      </RouterLink>
    </div>

    <!-- Rooms section -->
    <div v-if="joinedRoomList.length > 0" class="flex flex-col px-2 pt-2">
      <span class="mb-1 px-2 text-xs font-semibold uppercase tracking-wide text-muted">Rooms</span>
      <ul class="flex flex-col gap-0.5">
        <li v-for="roomJid in joinedRoomList" :key="roomJid">
          <button
            class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm transition-colors hover:bg-hover"
            :class="isActiveChat(roomJid) ? 'bg-active text-foreground' : 'text-muted'"
            @click="openChat(roomJid)"
          >
            <span class="text-base">#</span>
            <span class="truncate">{{ roomLocalpart(roomJid) }}</span>
          </button>
        </li>
      </ul>
    </div>

    <!-- DM section header -->
    <div class="flex items-center justify-between px-4 pb-1 pt-4">
      <span class="text-xs font-semibold uppercase tracking-wide text-muted">Direct Messages</span>
    </div>

    <!-- Conversation list -->
    <nav class="flex-1 overflow-y-auto px-2 pb-2">
      <ul class="flex flex-col gap-0.5">
        <li v-for="convo in conversations" :key="convo.jid">
          <button
            class="flex w-full items-center gap-3 rounded px-2 py-1.5 text-left transition-colors hover:bg-hover"
            :class="isActiveChat(convo.jid) ? 'bg-active' : ''"
            @click="openChat(convo.jid)"
          >
            <!-- Avatar -->
            <div
              class="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full text-xs font-semibold text-white"
              :style="{ backgroundColor: getAvatarColor(convo.jid) }"
            >
              {{ getInitials(convo.title) }}
            </div>
            <!-- Name + preview -->
            <div class="min-w-0 flex-1">
              <div class="flex items-center justify-between gap-1">
                <span
                  class="truncate text-sm font-medium"
                  :class="isActiveChat(convo.jid) ? 'text-foreground' : 'text-muted'"
                >
                  {{ convo.title }}
                </span>
                <span v-if="convo.updatedAt" class="flex-shrink-0 text-[10px] text-muted">
                  {{ formatTime(convo.updatedAt) }}
                </span>
              </div>
              <p v-if="convo.preview" class="truncate text-xs text-muted">{{ convo.preview }}</p>
            </div>
          </button>
        </li>
      </ul>

      <!-- New DM input -->
      <form class="mt-2 px-1" @submit.prevent="openDirectMessage">
        <input
          v-model="directJid"
          type="text"
          placeholder="Start a DM (user@host)"
          class="w-full rounded bg-background px-2 py-1.5 text-xs text-foreground placeholder-muted outline-none focus:ring-1 focus:ring-accent"
          autocomplete="off"
        />
      </form>
    </nav>

    <!-- Connection status + logout footer -->
    <div class="flex items-center justify-between border-t border-border px-3 py-2">
      <div class="flex items-center gap-2">
        <span class="h-2 w-2 rounded-full" :class="statusDotClass"></span>
        <span class="text-xs text-muted capitalize">{{ connectionStatus }}</span>
      </div>
      <button
        class="rounded px-2 py-1 text-xs text-muted transition-colors hover:bg-hover hover:text-foreground"
        title="Sign out"
        @click="handleLogout"
      >
        <svg class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M17 16l4-4m0 0l-4-4m4 4H7m6 4v1a3 3 0 01-3 3H6a3 3 0 01-3-3V7a3 3 0 013-3h4a3 3 0 013 3v1" />
        </svg>
      </button>
    </div>
  </aside>
</template>

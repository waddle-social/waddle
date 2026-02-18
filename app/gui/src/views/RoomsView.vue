<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { storeToRefs } from 'pinia';
import { useRoomsStore } from '../stores/rooms';

const router = useRouter();
const roomsStore = useRoomsStore();
const { rooms, joinedRooms, loading, error } = storeToRefs(roomsStore);

const newRoomName = ref('');
const creating = ref(false);
const createError = ref<string | null>(null);
const actionError = ref<string | null>(null);

function isJoined(roomJid: string): boolean {
  return joinedRooms.value.has(roomJid);
}

function roomLocalpart(jid: string): string {
  return jid.split('@')[0] || jid;
}

function getAvatarColor(jid: string): string {
  const colors = ['#5865f2', '#57f287', '#fee75c', '#eb459e', '#ed4245', '#3ba55c', '#faa61a', '#e67e22'];
  let hash = 0;
  for (const ch of jid) hash = ch.charCodeAt(0) + ((hash << 5) - hash);
  return colors[Math.abs(hash) % colors.length] ?? colors[0] ?? '#5865f2';
}

function getInitials(name: string): string {
  return name.slice(0, 2).toUpperCase();
}

async function handleJoin(roomJid: string): Promise<void> {
  actionError.value = null;
  try {
    await roomsStore.join(roomJid);
  } catch (err) {
    actionError.value = err instanceof Error ? err.message : String(err);
  }
}

async function handleLeave(roomJid: string): Promise<void> {
  actionError.value = null;
  try {
    await roomsStore.leave(roomJid);
  } catch (err) {
    actionError.value = err instanceof Error ? err.message : String(err);
  }
}

async function handleCreate(): Promise<void> {
  const name = newRoomName.value.trim();
  if (!name || creating.value) return;
  creating.value = true;
  createError.value = null;
  try {
    await roomsStore.create(name);
    newRoomName.value = '';
  } catch (err) {
    createError.value = err instanceof Error ? err.message : String(err);
  } finally {
    creating.value = false;
  }
}

async function handleDelete(roomJid: string): Promise<void> {
  actionError.value = null;
  if (!confirm(`Delete room ${roomLocalpart(roomJid)}? This cannot be undone.`)) return;
  try {
    await roomsStore.destroy(roomJid);
  } catch (err) {
    actionError.value = err instanceof Error ? err.message : String(err);
  }
}

function openRoom(roomJid: string): void {
  void router.push(`/chat/${encodeURIComponent(roomJid)}`);
}

onMounted(() => {
  roomsStore.startListening();
});

onUnmounted(() => {
  roomsStore.stopListening();
});
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Header -->
    <header class="flex h-12 flex-shrink-0 items-center justify-between border-b border-border px-4 shadow-sm">
      <div class="flex items-center gap-2">
        <svg class="h-5 w-5 text-muted" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M7 20l4-16m2 16l4-16M6 9h14M4 15h14" />
        </svg>
        <h2 class="text-base font-semibold text-foreground">Rooms</h2>
        <span v-if="rooms.length" class="text-xs text-muted">{{ rooms.length }} available</span>
      </div>
      <button
        class="rounded px-2 py-1 text-xs text-muted transition-colors hover:bg-hover hover:text-foreground"
        @click="roomsStore.fetchRooms()"
      >
        Refresh
      </button>
    </header>

    <div class="flex-1 overflow-y-auto p-4">
      <!-- Create room form -->
      <form class="mb-4 flex gap-2" @submit.prevent="handleCreate">
        <input
          v-model="newRoomName"
          type="text"
          placeholder="Create a new room…"
          class="min-w-0 flex-1 rounded-lg bg-surface-raised px-3 py-2 text-sm text-foreground placeholder-muted outline-none focus:ring-1 focus:ring-accent"
          autocomplete="off"
        />
        <button
          type="submit"
          class="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-accent/80 disabled:opacity-50"
          :disabled="creating || !newRoomName.trim()"
        >
          {{ creating ? 'Creating…' : 'Create' }}
        </button>
      </form>

      <!-- Errors -->
      <div v-if="error" class="mb-4 rounded bg-danger/20 px-3 py-2 text-sm text-danger">
        {{ error }}
      </div>
      <div v-if="createError" class="mb-4 rounded bg-danger/20 px-3 py-2 text-sm text-danger">
        {{ createError }}
      </div>
      <div v-if="actionError" class="mb-4 rounded bg-danger/20 px-3 py-2 text-sm text-danger">
        {{ actionError }}
      </div>

      <!-- Loading -->
      <p v-if="loading" class="text-sm text-muted">Discovering rooms…</p>

      <!-- Empty state -->
      <p v-else-if="rooms.length === 0" class="text-sm text-muted">
        No rooms found. Create one above to get started.
      </p>

      <!-- Room list -->
      <ul v-else class="space-y-1">
        <li
          v-for="room in rooms"
          :key="room.jid"
          class="group flex items-center gap-3 rounded px-2 py-2 transition-colors hover:bg-hover"
        >
          <!-- Avatar -->
          <div
            class="flex h-10 w-10 flex-shrink-0 items-center justify-center rounded-lg text-sm font-semibold text-white"
            :style="{ backgroundColor: getAvatarColor(room.jid) }"
          >
            {{ getInitials(room.name || roomLocalpart(room.jid)) }}
          </div>

          <!-- Info -->
          <div
            class="min-w-0 flex-1 cursor-pointer"
            @click="isJoined(room.jid) ? openRoom(room.jid) : handleJoin(room.jid)"
          >
            <p class="truncate text-sm font-medium text-foreground">
              {{ room.name || roomLocalpart(room.jid) }}
            </p>
            <p class="truncate text-xs text-muted">{{ room.jid }}</p>
          </div>

          <!-- Actions -->
          <div class="flex items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
            <template v-if="isJoined(room.jid)">
              <button
                class="rounded bg-surface-raised px-2 py-1 text-xs text-foreground transition-colors hover:bg-hover"
                @click="openRoom(room.jid)"
              >
                Open
              </button>
              <button
                class="rounded bg-surface-raised px-2 py-1 text-xs text-muted transition-colors hover:bg-hover hover:text-foreground"
                @click="handleLeave(room.jid)"
              >
                Leave
              </button>
            </template>
            <template v-else>
              <button
                class="rounded bg-accent px-2 py-1 text-xs text-white transition-colors hover:bg-accent/80"
                @click="handleJoin(room.jid)"
              >
                Join
              </button>
            </template>
            <button
              class="rounded bg-surface-raised px-2 py-1 text-xs text-danger transition-colors hover:bg-danger/20"
              title="Delete room (owner only)"
              @click="handleDelete(room.jid)"
            >
              ✕
            </button>
          </div>
        </li>
      </ul>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { Plus, Lock, Compass } from "lucide-vue-next";
import type { WaddleSummary } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";

const props = defineProps<{
  waddles: WaddleSummary[];
  activeWaddleId: string | null;
  session: WaddleSession | null;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
}>();

const emit = defineEmits<{
  selectWaddle: [id: string];
  createWaddle: [];
  browsePublicWaddles: [];
  logout: [];
  "request-notifications": [];
  "toggle-notifications": [];
}>();

const publicWaddles = computed(() =>
  props.waddles.filter((w) => w.is_public),
);

const privateWaddles = computed(() =>
  props.waddles.filter((w) => !w.is_public),
);

function waddleInitial(waddle: WaddleSummary): string {
  return (waddle.name[0] ?? "W").toUpperCase();
}

function waddleColor(waddle: WaddleSummary): string {
  const colors = [
    "#5e6ad2", "#e5484d", "#30a46c", "#f5a623",
    "#0091ff", "#7c66dc", "#e54666", "#12a594",
    "#e5484d", "#6e56cf",
  ];
  let hash = 0;
  for (const char of waddle.id) hash = ((hash << 5) - hash + char.charCodeAt(0)) | 0;
  return colors[Math.abs(hash) % colors.length];
}
</script>

<template>
  <div class="w-[60px] bg-rail flex flex-col items-center flex-shrink-0 py-3 gap-1">
    <!-- Logo -->
    <div class="w-9 h-9 flex items-center justify-center mb-2">
      <span class="text-lg">🐧</span>
    </div>

    <!-- Divider -->
    <div class="w-6 h-px bg-rail-foreground/20 mb-1" />

    <!-- Waddle icons -->
    <div class="flex-1 overflow-auto flex flex-col items-center gap-1 w-full px-2">
      <button
        v-for="waddle in privateWaddles"
        :key="waddle.id"
        class="relative w-9 h-9 rounded-lg flex items-center justify-center text-xs font-semibold transition-all group"
        :class="activeWaddleId === waddle.id
          ? 'bg-primary text-primary-foreground shadow-sm'
          : 'text-rail-foreground hover:bg-rail-hover hover:text-rail-active'"
        :title="waddle.name"
        @click="emit('selectWaddle', waddle.id)"
      >
        <span>{{ waddleInitial(waddle) }}</span>
        <!-- Active indicator -->
        <span
          v-if="activeWaddleId === waddle.id"
          class="absolute -left-2 top-1/2 -translate-y-1/2 w-[3px] h-5 bg-primary-foreground rounded-r-full"
        />
        <!-- Lock badge -->
        <Lock class="absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 text-rail-foreground/50" />
      </button>

      <template v-if="publicWaddles.length > 0 && privateWaddles.length > 0">
        <div class="w-6 h-px bg-rail-foreground/15 my-1" />
      </template>

      <button
        v-for="waddle in publicWaddles"
        :key="waddle.id"
        class="relative w-9 h-9 rounded-lg flex items-center justify-center text-xs font-semibold transition-all"
        :class="activeWaddleId === waddle.id
          ? 'bg-primary text-primary-foreground shadow-sm'
          : 'text-rail-foreground hover:bg-rail-hover hover:text-rail-active'"
        :title="waddle.name"
        @click="emit('selectWaddle', waddle.id)"
      >
        <span>{{ waddleInitial(waddle) }}</span>
        <span
          v-if="activeWaddleId === waddle.id"
          class="absolute -left-2 top-1/2 -translate-y-1/2 w-[3px] h-5 bg-primary-foreground rounded-r-full"
        />
      </button>

      <div v-if="waddles.length === 0" class="text-rail-foreground/30 text-[10px] mt-2 text-center">
        No waddles
      </div>
    </div>

    <!-- Bottom actions -->
    <div class="flex flex-col items-center gap-1 mt-1">
      <button
        class="w-9 h-9 flex items-center justify-center rounded-lg text-rail-foreground hover:bg-rail-hover hover:text-rail-active transition-colors"
        title="Browse public spaces"
        @click="emit('browsePublicWaddles')"
      >
        <Compass class="w-4 h-4" />
      </button>
      <button
        class="w-9 h-9 flex items-center justify-center rounded-lg text-rail-foreground hover:bg-rail-hover hover:text-rail-active transition-colors"
        title="Create waddle"
        @click="emit('createWaddle')"
      >
        <Plus class="w-4 h-4" />
      </button>
    </div>

    <!-- Profile footer -->
    <ProfilePanel
      v-if="session"
      :session="session"
      :notification-permission="notificationPermission"
      :notifications-enabled="notificationsEnabled"
      compact
      @logout="emit('logout')"
      @request-notifications="emit('request-notifications')"
      @toggle-notifications="emit('toggle-notifications')"
    />
  </div>
</template>

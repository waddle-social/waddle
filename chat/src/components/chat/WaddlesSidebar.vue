<script setup lang="ts">
import { computed } from "vue";
import { Plus, Lock, Compass, MessageCircle } from "lucide-vue-next";
import type { WaddleSummary } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";

const props = defineProps<{
  waddles: WaddleSummary[];
  activeWaddleId: string | null;
  activeSidebarMode?: "channels" | "dms";
  hasUnreadDms?: boolean;
  session: WaddleSession | null;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
  horizontal?: boolean;
}>();

const emit = defineEmits<{
  selectWaddle: [id: string];
  createWaddle: [];
  browsePublicWaddles: [];
  toggleDms: [];
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
    "#00ddc0", "#ef4444", "#10b981", "#f59e0b",
    "#3b82f6", "#8b5cf6", "#ec4899", "#14b8a6",
    "#f97316", "#6366f1",
  ];
  let hash = 0;
  for (const char of waddle.id) hash = ((hash << 5) - hash + char.charCodeAt(0)) | 0;
  return colors[Math.abs(hash) % colors.length];
}
</script>

<template>
  <div
    class="bg-rail flex flex-shrink-0"
    :class="horizontal
      ? 'w-full flex-row items-center px-3 py-2 gap-2'
      : 'w-[64px] flex-col items-center py-4 gap-1.5'"
  >
    <!-- Logo -->
    <div class="w-10 h-10 flex items-center justify-center flex-shrink-0" :class="horizontal ? '' : 'mb-2'">
      <span class="text-xl">🐧</span>
    </div>

    <!-- Divider -->
    <div class="bg-border flex-shrink-0" :class="horizontal ? 'h-7 w-px' : 'w-7 h-px mb-1'" />

    <!-- Waddle icons -->
    <div
      class="flex-1 overflow-auto flex gap-1.5"
      :class="horizontal
        ? 'flex-row items-center min-w-0 py-0.5'
        : 'flex-col items-center w-full px-2'"
    >
      <button
        v-for="waddle in privateWaddles"
        :key="waddle.id"
        class="relative w-10 h-10 rounded-xl flex items-center justify-center text-xs font-semibold transition-all duration-200 group flex-shrink-0"
        :class="activeWaddleId === waddle.id
          ? 'text-primary-foreground shadow-lg'
          : 'text-rail-foreground hover:text-rail-active hover:scale-105'"
        :style="activeWaddleId === waddle.id
          ? { backgroundColor: waddleColor(waddle), boxShadow: `0 0 16px ${waddleColor(waddle)}40` }
          : { backgroundColor: 'var(--rail-hover)' }"
        :title="waddle.name"
        @click="emit('selectWaddle', waddle.id)"
      >
        <span class="font-display font-bold">{{ waddleInitial(waddle) }}</span>
        <!-- Active indicator -->
        <span
          v-if="activeWaddleId === waddle.id && !horizontal"
          class="absolute -left-2 top-1/2 -translate-y-1/2 w-[3px] h-5 rounded-r-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
        <span
          v-if="activeWaddleId === waddle.id && horizontal"
          class="absolute -bottom-2 left-1/2 -translate-x-1/2 h-[3px] w-5 rounded-t-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
        <!-- Lock badge -->
        <Lock class="absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 text-muted-foreground/50" />
      </button>

      <template v-if="publicWaddles.length > 0 && privateWaddles.length > 0">
        <div class="bg-border flex-shrink-0" :class="horizontal ? 'h-7 w-px mx-0.5' : 'w-7 h-px my-0.5'" />
      </template>

      <button
        v-for="waddle in publicWaddles"
        :key="waddle.id"
        class="relative w-10 h-10 rounded-xl flex items-center justify-center text-xs font-semibold transition-all duration-200 flex-shrink-0"
        :class="activeWaddleId === waddle.id
          ? 'text-primary-foreground shadow-lg'
          : 'text-rail-foreground hover:text-rail-active hover:scale-105'"
        :style="activeWaddleId === waddle.id
          ? { backgroundColor: waddleColor(waddle), boxShadow: `0 0 16px ${waddleColor(waddle)}40` }
          : { backgroundColor: 'var(--rail-hover)' }"
        :title="waddle.name"
        @click="emit('selectWaddle', waddle.id)"
      >
        <span class="font-display font-bold">{{ waddleInitial(waddle) }}</span>
        <span
          v-if="activeWaddleId === waddle.id && !horizontal"
          class="absolute -left-2 top-1/2 -translate-y-1/2 w-[3px] h-5 rounded-r-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
        <span
          v-if="activeWaddleId === waddle.id && horizontal"
          class="absolute -bottom-2 left-1/2 -translate-x-1/2 h-[3px] w-5 rounded-t-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
      </button>

      <div v-if="waddles.length === 0" class="text-muted-foreground/30 text-[10px] text-center" :class="horizontal ? '' : 'mt-3'">
        No waddles
      </div>
    </div>

    <!-- Bottom actions -->
    <div class="flex gap-1.5 flex-shrink-0" :class="horizontal ? 'flex-row items-center' : 'flex-col items-center mt-2'">
      <button
        class="relative w-10 h-10 flex items-center justify-center rounded-xl transition-all duration-200 hover:scale-105"
        :class="activeSidebarMode === 'dms'
          ? 'bg-rail-hover text-primary'
          : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
        title="Direct messages"
        @click="emit('toggleDms')"
      >
        <MessageCircle class="w-4 h-4" />
        <span
          v-if="hasUnreadDms"
          class="absolute -top-0.5 -right-0.5 w-2 h-2 rounded-full bg-primary shadow-[0_0_6px_var(--glow-strong)]"
        />
      </button>
      <button
        class="w-10 h-10 flex items-center justify-center rounded-xl text-rail-foreground hover:bg-rail-hover hover:text-primary transition-all duration-200 hover:scale-105"
        title="Browse public spaces"
        @click="emit('browsePublicWaddles')"
      >
        <Compass class="w-4 h-4" />
      </button>
      <button
        class="w-10 h-10 flex items-center justify-center rounded-xl text-rail-foreground hover:bg-rail-hover hover:text-primary transition-all duration-200 hover:scale-105"
        title="Create waddle"
        @click="emit('createWaddle')"
      >
        <Plus class="w-4 h-4" />
      </button>
    </div>

    <!-- Profile footer (vertical only) -->
    <ProfilePanel
      v-if="session && !horizontal"
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

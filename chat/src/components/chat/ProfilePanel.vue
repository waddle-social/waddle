<script setup lang="ts">
import { computed } from "vue";
import { Bell, BellOff, LogOut } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { WaddleSession } from "@/lib/server-auth";

const props = defineProps<{
  session: WaddleSession;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
  compact?: boolean;
}>();

const emit = defineEmits<{
  logout: [];
  "request-notifications": [];
  "toggle-notifications": [];
}>();

const bellTitle = computed(() => {
  if (props.notificationPermission === "denied") return "Notifications blocked — update browser settings";
  if (props.notificationPermission === "granted" && props.notificationsEnabled) return "Notifications enabled";
  if (props.notificationPermission === "granted") return "Notifications disabled";
  return "Enable notifications";
});

function handleBellClick() {
  if (props.notificationPermission === "denied") return;
  if (props.notificationPermission === "granted") {
    emit("toggle-notifications");
  } else {
    emit("request-notifications");
  }
}
</script>

<template>
  <!-- Compact mode for icon rail -->
  <div v-if="compact" class="flex flex-col items-center gap-1.5 pt-3 border-t border-border mt-2">
    <button
      class="w-10 h-10 flex items-center justify-center rounded-xl transition-all duration-200"
      :class="notificationPermission === 'denied'
        ? 'opacity-30 cursor-not-allowed text-rail-foreground'
        : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
      :title="bellTitle"
      :aria-label="bellTitle"
      :disabled="notificationPermission === 'denied'"
      @click="handleBellClick"
    >
      <BellOff v-if="notificationPermission === 'denied' || (notificationPermission === 'granted' && !notificationsEnabled)" class="w-3.5 h-3.5" />
      <Bell v-else class="w-3.5 h-3.5" />
    </button>
    <button
      class="w-10 h-10 flex items-center justify-center rounded-xl text-rail-foreground hover:bg-rail-hover hover:text-rail-active transition-all duration-200"
      :title="`${session.username} — Log out`"
      @click="emit('logout')"
    >
      <AppAvatar :name="session.username" :src="session.avatar_url" size="xs" />
    </button>
  </div>

  <!-- Full mode for sidebar -->
  <div v-else class="px-3 py-2.5 flex items-center gap-2.5 flex-shrink-0 border-t border-border">
    <AppAvatar :name="session.username" :src="session.avatar_url" size="sm" />
    <span class="flex-1 min-w-0 text-[13px] font-medium truncate text-sidebar-foreground">{{ session.username }}</span>
    <button
      class="h-7 w-7 flex items-center justify-center rounded-lg transition-all duration-200 flex-shrink-0"
      :class="notificationPermission === 'denied'
        ? 'opacity-30 cursor-not-allowed'
        : 'hover:bg-sidebar-accent text-sidebar-muted hover:text-primary'"
      :title="bellTitle"
      :aria-label="bellTitle"
      :disabled="notificationPermission === 'denied'"
      @click="handleBellClick"
    >
      <BellOff v-if="notificationPermission === 'denied' || (notificationPermission === 'granted' && !notificationsEnabled)" class="w-3.5 h-3.5" />
      <Bell v-else class="w-3.5 h-3.5" />
    </button>
    <button
      class="h-7 w-7 flex items-center justify-center rounded-lg text-sidebar-muted hover:bg-sidebar-accent hover:text-destructive transition-all duration-200 flex-shrink-0"
      title="Log out"
      @click="emit('logout')"
    >
      <LogOut class="w-3.5 h-3.5" />
    </button>
  </div>
</template>

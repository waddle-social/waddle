<script setup lang="ts">
import { computed } from "vue";
import { Bell, BellOff, LogOut } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { WaddleSession } from "@/lib/server-auth";

const props = defineProps<{
  session: WaddleSession;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
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
  <div class="h-16 border-t border-foreground px-6 flex items-center gap-3 flex-shrink-0">
    <AppAvatar :name="session.username" size="sm" />
    <span class="flex-1 min-w-0 text-sm font-mono font-bold truncate">{{ session.username }}</span>
    <button
      class="h-7 w-7 flex items-center justify-center transition-colors flex-shrink-0"
      :class="notificationPermission === 'denied'
        ? 'opacity-30 cursor-not-allowed'
        : 'hover:bg-foreground/10'"
      :title="bellTitle"
      :aria-label="bellTitle"
      :disabled="notificationPermission === 'denied'"
      @click="handleBellClick"
    >
      <BellOff v-if="notificationPermission === 'denied' || (notificationPermission === 'granted' && !notificationsEnabled)" class="w-3.5 h-3.5" />
      <Bell v-else class="w-3.5 h-3.5" />
    </button>
    <button
      class="h-7 w-7 flex items-center justify-center hover:bg-destructive hover:text-destructive-foreground transition-colors flex-shrink-0"
      title="Log out"
      @click="emit('logout')"
    >
      <LogOut class="w-3.5 h-3.5" />
    </button>
  </div>
</template>

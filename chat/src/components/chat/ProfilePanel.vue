<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { Bell, BellOff, LogOut, Settings } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ThemeSwitcher from "@/components/chat/ThemeSwitcher.vue";
import VersionFooter from "@/components/chat/VersionFooter.vue";
import type { ServerVersion } from "@/composables/useVersion";
import type { WaddleSession } from "@/lib/server-auth";

const props = defineProps<{
  session: WaddleSession;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
  totalUnreadCount?: number;
  totalMentionCount?: number;
  compact?: boolean;
  webCommitSha?: string;
  serverVersion?: ServerVersion | null;
}>();

const emit = defineEmits<{
  logout: [];
  "open-settings": [];
  "request-notifications": [];
  "toggle-notifications": [];
}>();

const bellTitle = computed(() => {
  if (props.notificationPermission === "denied") return "Notifications blocked — update browser settings";
  if (props.notificationPermission === "granted" && props.notificationsEnabled) return "Notifications enabled";
  if (props.notificationPermission === "granted") return "Notifications disabled";
  return "Enable notifications";
});

const detailsOpen = ref(false);
const compactRootEl = ref<HTMLElement | null>(null);

function handleBellClick() {
  if (props.notificationPermission === "denied") return;
  if (props.notificationPermission === "granted") {
    emit("toggle-notifications");
  } else {
    emit("request-notifications");
  }
}

function toggleDetails() {
  detailsOpen.value = !detailsOpen.value;
}

function closeDetails() {
  detailsOpen.value = false;
}

function handleOpenSettings() {
  closeDetails();
  emit("open-settings");
}

function handleLogout() {
  closeDetails();
  emit("logout");
}

function onWindowClick(event: MouseEvent) {
  if (!props.compact || !detailsOpen.value) return;
  const target = event.target as Node | null;
  if (!compactRootEl.value || !target) return;
  if (!compactRootEl.value.contains(target)) closeDetails();
}

function onEsc(event: KeyboardEvent) {
  if (event.key === "Escape") closeDetails();
}

watch(
  () => props.compact,
  (compact) => {
    if (!compact) closeDetails();
  },
);

onMounted(() => {
  window.addEventListener("mousedown", onWindowClick);
  window.addEventListener("keydown", onEsc);
});

onUnmounted(() => {
  window.removeEventListener("mousedown", onWindowClick);
  window.removeEventListener("keydown", onEsc);
});
</script>

<template>
  <div v-if="compact" ref="compactRootEl" class="relative mt-2 flex flex-col items-center gap-1.5 border-t border-border pt-3">
    <button
      class="relative flex h-10 w-10 items-center justify-center rounded-xl transition-all duration-200"
      :class="notificationPermission === 'denied'
        ? 'cursor-not-allowed text-rail-foreground opacity-30'
        : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
      :title="bellTitle"
      :aria-label="bellTitle"
      :disabled="notificationPermission === 'denied'"
      @click="handleBellClick"
    >
      <BellOff v-if="notificationPermission === 'denied' || (notificationPermission === 'granted' && !notificationsEnabled)" class="h-3.5 w-3.5" />
      <Bell v-else class="h-3.5 w-3.5" />
      <span
        v-if="(totalMentionCount ?? 0) > 0"
        class="absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full text-[8px] font-bold bg-destructive text-destructive-foreground"
      >{{ totalMentionCount }}</span>
      <span
        v-else-if="(totalUnreadCount ?? 0) > 0"
        class="absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full text-[8px] font-bold bg-primary text-primary-foreground"
      >{{ totalUnreadCount }}</span>
    </button>
    <button
      class="flex h-10 w-10 items-center justify-center rounded-xl text-rail-foreground transition-all duration-200 hover:bg-rail-hover hover:text-rail-active"
      :title="`${session.username} — Account and build details`"
      aria-haspopup="dialog"
      :aria-expanded="detailsOpen"
      @click="toggleDetails"
    >
      <AppAvatar :name="session.username" :src="session.avatar_url" size="xs" />
    </button>
    <div
      v-if="detailsOpen"
      class="animate-fade-in absolute bottom-0 left-full z-20 ml-3 w-72 rounded-lg border border-border bg-popover/95 p-2 text-popover-foreground shadow-xl backdrop-blur-xl"
      role="dialog"
      :aria-label="`${session.username} account menu`"
    >
      <div class="flex items-center gap-3 rounded-lg bg-muted/30 px-2.5 py-2">
        <AppAvatar :name="session.username" :src="session.avatar_url" size="sm" />
        <div class="min-w-0 flex-1">
          <div class="truncate text-[13px] font-semibold">{{ session.username }}</div>
          <div class="text-[11px] text-muted-foreground">Signed in</div>
        </div>
      </div>
      <div class="mt-2 rounded-lg border border-border/70 bg-background/50 px-2.5 py-2">
        <ThemeSwitcher />
      </div>
      <div class="mt-2 border-t border-border pt-2">
        <button
          class="flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-[12px] font-medium text-foreground transition-colors duration-200 hover:bg-muted"
          @click="handleOpenSettings"
        >
          <Settings class="h-3.5 w-3.5 text-primary/70" />
          <span class="min-w-0 flex-1">Settings</span>
        </button>
      </div>
      <div class="mt-2 border-t border-border pt-2">
        <VersionFooter
          :web-commit-sha="webCommitSha"
          :server-version="serverVersion"
          layout="detail"
        />
      </div>
      <button
        class="mt-2 flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-[12px] font-medium text-muted-foreground transition-colors duration-200 hover:bg-muted hover:text-destructive"
        @click="handleLogout"
      >
        <LogOut class="h-3.5 w-3.5" />
        <span>Log out</span>
      </button>
    </div>
  </div>

  <div v-else class="flex flex-shrink-0 flex-col gap-2.5 border-t border-border px-3 py-2.5">
    <div class="flex items-center gap-2">
      <button
        class="flex min-w-0 flex-1 items-center gap-2.5 rounded-xl px-1.5 py-1.5 text-left transition-all duration-200 hover:bg-sidebar-accent"
        title="Open settings"
        @click="handleOpenSettings"
      >
        <AppAvatar :name="session.username" :src="session.avatar_url" size="sm" />
        <span class="min-w-0 flex-1 truncate text-[13px] font-medium text-sidebar-foreground">{{ session.username }}</span>
        <Settings class="h-3.5 w-3.5 flex-shrink-0 text-sidebar-muted" />
      </button>
      <button
        class="relative flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-lg transition-all duration-200"
        :class="notificationPermission === 'denied'
          ? 'cursor-not-allowed opacity-30'
          : 'text-sidebar-muted hover:bg-sidebar-accent hover:text-primary'"
        :title="bellTitle"
        :aria-label="bellTitle"
        :disabled="notificationPermission === 'denied'"
        @click="handleBellClick"
      >
        <BellOff v-if="notificationPermission === 'denied' || (notificationPermission === 'granted' && !notificationsEnabled)" class="h-3.5 w-3.5" />
        <Bell v-else class="h-3.5 w-3.5" />
        <span
          v-if="(totalMentionCount ?? 0) > 0"
          class="absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full text-[8px] font-bold bg-destructive text-destructive-foreground"
        >{{ totalMentionCount }}</span>
        <span
          v-else-if="(totalUnreadCount ?? 0) > 0"
          class="absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full text-[8px] font-bold bg-primary text-primary-foreground"
        >{{ totalUnreadCount }}</span>
      </button>
      <button
        class="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-lg text-sidebar-muted transition-all duration-200 hover:bg-sidebar-accent hover:text-destructive"
        title="Log out"
        @click="handleLogout"
      >
        <LogOut class="h-3.5 w-3.5" />
      </button>
    </div>
    <div class="rounded-lg border border-border/70 bg-muted/30 px-2.5 py-2">
      <ThemeSwitcher />
    </div>
    <div class="border-t border-border pt-2">
      <VersionFooter
        :web-commit-sha="webCommitSha"
        :server-version="serverVersion"
        layout="inline"
      />
    </div>
  </div>
</template>

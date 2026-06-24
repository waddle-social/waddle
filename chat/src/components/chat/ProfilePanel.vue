<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { Bell, BellOff, ChevronUp, LogOut, Settings, Volume2, VolumeX } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import PresencePicker from "@/components/chat/PresencePicker.vue";
import ThemeSwitcher from "@/components/chat/ThemeSwitcher.vue";
import VersionFooter from "@/components/chat/VersionFooter.vue";
import type { XmppServerVersion } from "@/shell/version";
import type { WaddleSession } from "@/lib/server-auth";

const props = defineProps<{
  session: WaddleSession;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
  messageSoundsEnabled?: boolean;
  totalUnreadCount?: number;
  totalMentionCount?: number;
  compact?: boolean;
  webCommitSha?: string;
  serverVersion?: XmppServerVersion | null;
}>();

const emit = defineEmits<{
  logout: [];
  "open-settings": [];
  "request-notifications": [];
  "toggle-notifications": [];
  "toggle-message-sounds": [];
}>();

const bellTitle = computed(() => {
  if (props.notificationPermission === "denied") return "Notifications blocked — update browser settings";
  if (props.notificationPermission === "granted" && props.notificationsEnabled) return "Notifications enabled";
  if (props.notificationPermission === "granted") return "Notifications disabled";
  return "Enable notifications";
});

const bellAriaLabel = computed(() => {
  const base = bellTitle.value;
  if ((props.totalMentionCount ?? 0) > 0) return `${base}, ${props.totalMentionCount} unread mention${props.totalMentionCount === 1 ? "" : "s"} in channels`;
  if ((props.totalUnreadCount ?? 0) > 0) return `${base}, ${props.totalUnreadCount} unread message${props.totalUnreadCount === 1 ? "" : "s"} in channels`;
  return base;
});

const detailsOpen = ref(false);
const accountMenuOpen = ref(false);
const compactRootEl = ref<HTMLElement | null>(null);
const expandedMenuRootEl = ref<HTMLElement | null>(null);

function handleBellClick() {
  if (props.notificationPermission === "denied") return;
  if (props.notificationPermission === "granted") {
    emit("toggle-notifications");
  } else {
    emit("request-notifications");
  }
}

function handleToggleMessageSounds() {
  emit("toggle-message-sounds");
}

function toggleDetails() {
  detailsOpen.value = !detailsOpen.value;
}

function closeDetails() {
  detailsOpen.value = false;
}

function toggleAccountMenu() {
  accountMenuOpen.value = !accountMenuOpen.value;
}

function closeAccountMenu() {
  accountMenuOpen.value = false;
}

function handleOpenSettings() {
  closeDetails();
  closeAccountMenu();
  emit("open-settings");
}

function handleLogout() {
  closeDetails();
  closeAccountMenu();
  emit("logout");
}

function onWindowClick(event: MouseEvent) {
  const target = event.target as Node | null;
  if (!target) return;
  if (props.compact && detailsOpen.value && compactRootEl.value && !compactRootEl.value.contains(target)) {
    closeDetails();
  }
  if (!props.compact && accountMenuOpen.value && expandedMenuRootEl.value && !expandedMenuRootEl.value.contains(target)) {
    closeAccountMenu();
  }
}

function onEsc(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  closeDetails();
  closeAccountMenu();
}

watch(
  () => props.compact,
  (compact) => {
    if (compact) closeAccountMenu();
    else closeDetails();
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
  <div v-if="compact" ref="compactRootEl" class="relative flex w-full flex-col items-center gap-2 border-t border-border pt-3">
    <button
      class="relative flex h-10 w-10 items-center justify-center rounded-lg transition-all duration-200"
      type="button"
      :class="notificationPermission === 'denied'
        ? 'cursor-not-allowed text-rail-foreground opacity-30'
        : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
      :title="bellTitle"
      :aria-label="bellAriaLabel"
      :disabled="notificationPermission === 'denied'"
      @click="handleBellClick"
    >
      <BellOff v-if="notificationPermission === 'denied' || (notificationPermission === 'granted' && !notificationsEnabled)" class="h-3.5 w-3.5" />
      <Bell v-else class="h-3.5 w-3.5" />
      <!-- Bell-corner count badges pick up the same glow halo the
           sidebar unread/mention badges use (iter 39 brand
           language). The bell is the global unread/mention indicator
           — it should pull the eye at least as much as a single row. -->
      <span
        v-if="(totalMentionCount ?? 0) > 0"
        class="chat-badge-glow--mention type-count-badge absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
        aria-hidden="true"
      >{{ totalMentionCount }}</span>
      <span
        v-else-if="(totalUnreadCount ?? 0) > 0"
        class="chat-badge-glow--primary type-count-badge absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full bg-primary text-primary-foreground"
        aria-hidden="true"
      >{{ totalUnreadCount }}</span>
    </button>
    <button
      class="flex h-10 w-10 items-center justify-center rounded-lg text-rail-foreground transition-all duration-200 hover:bg-rail-hover hover:text-rail-active"
      type="button"
      :title="`${session.username} — Account menu`"
      aria-haspopup="menu"
      :aria-expanded="detailsOpen"
      @click="toggleDetails"
    >
      <AppAvatar :name="session.username" :src="session.avatar_url" size="xs" />
    </button>
    <div
      v-if="detailsOpen"
      class="z-floating animate-fade-in absolute bottom-0 left-full ml-3 w-[var(--chat-profile-menu-width)] rounded-lg border border-border bg-popover/95 p-2 text-popover-foreground shadow-xl backdrop-blur-xl"
      role="dialog"
      :aria-label="`${session.username} account menu`"
    >
      <div class="flex min-h-12 items-center gap-3 rounded-lg bg-muted/30 px-2.5 py-2">
        <AppAvatar :name="session.username" :src="session.avatar_url" size="sm" />
        <div class="min-w-0 flex-1">
          <div class="type-menu-title truncate">{{ session.username }}</div>
          <div class="type-meta text-muted-foreground">Signed in</div>
        </div>
        <ThemeSwitcher />
      </div>
      <div class="mt-2 border-t border-border pt-2">
        <PresencePicker />
      </div>
      <div class="mt-2 flex flex-col gap-1 border-t border-border pt-2">
        <button
          class="type-menu-item flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-foreground transition-colors duration-200 hover:bg-muted"
          type="button"
          @click="handleOpenSettings"
        >
          <Settings class="h-3.5 w-3.5 text-primary/70" />
          <span>Settings</span>
        </button>
        <button
          class="type-menu-item flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-foreground transition-colors duration-200 hover:bg-muted"
          type="button"
          :aria-pressed="messageSoundsEnabled !== false"
          @click="handleToggleMessageSounds"
        >
          <Volume2 v-if="messageSoundsEnabled !== false" class="h-3.5 w-3.5 text-primary/70" />
          <VolumeX v-else class="h-3.5 w-3.5 text-muted-foreground" />
          <span>{{ messageSoundsEnabled !== false ? "Message sounds on" : "Message sounds off" }}</span>
        </button>
        <button
          class="type-menu-item flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-muted-foreground transition-colors duration-200 hover:bg-muted hover:text-destructive"
          type="button"
          @click="handleLogout"
        >
          <LogOut class="h-3.5 w-3.5" />
          <span>Log out</span>
        </button>
      </div>
      <div class="mt-2 border-t border-border px-2.5 pt-2">
        <VersionFooter
          :web-commit-sha="webCommitSha"
          :server-version="serverVersion"
          layout="detail"
        />
      </div>
    </div>
  </div>

  <div v-else class="flex flex-shrink-0 flex-col gap-2.5 border-t border-border px-3 py-2.5">
    <div ref="expandedMenuRootEl" class="relative flex items-center gap-2">
      <button
        class="flex h-10 min-w-0 flex-1 items-center gap-2.5 rounded-lg px-1.5 text-left transition-all duration-200 hover:bg-sidebar-accent"
        type="button"
        :title="`${session.username} — Account menu`"
        aria-haspopup="menu"
        :aria-expanded="accountMenuOpen"
        @click="toggleAccountMenu"
      >
        <AppAvatar :name="session.username" :src="session.avatar_url" size="sm" />
        <span class="type-menu-title min-w-0 flex-1 truncate text-sidebar-foreground">{{ session.username }}</span>
        <ChevronUp
          class="h-3.5 w-3.5 flex-shrink-0 text-sidebar-muted transition-transform duration-200"
          :class="accountMenuOpen ? 'rotate-180' : ''"
        />
      </button>
      <button
        class="relative flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-lg transition-all duration-200"
        type="button"
        :class="notificationPermission === 'denied'
          ? 'cursor-not-allowed opacity-30'
          : 'text-sidebar-muted hover:bg-sidebar-accent hover:text-primary'"
        :title="bellTitle"
        :aria-label="bellAriaLabel"
        :disabled="notificationPermission === 'denied'"
        @click="handleBellClick"
      >
        <BellOff v-if="notificationPermission === 'denied' || (notificationPermission === 'granted' && !notificationsEnabled)" class="h-3.5 w-3.5" />
        <Bell v-else class="h-3.5 w-3.5" />
        <span
          v-if="(totalMentionCount ?? 0) > 0"
          class="type-count-badge absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
          aria-hidden="true"
        >{{ totalMentionCount }}</span>
        <span
          v-else-if="(totalUnreadCount ?? 0) > 0"
          class="type-count-badge absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full bg-primary text-primary-foreground"
          aria-hidden="true"
        >{{ totalUnreadCount }}</span>
      </button>
      <ThemeSwitcher />
      <div
        v-if="accountMenuOpen"
        class="z-floating animate-fade-in absolute bottom-full left-0 mb-2 flex w-[var(--chat-account-menu-width)] flex-col gap-1 rounded-lg border border-border bg-popover/95 p-2 text-popover-foreground shadow-xl backdrop-blur-xl"
        role="dialog"
        :aria-label="`${session.username} account menu`"
      >
        <PresencePicker />
        <div class="my-1 border-t border-border" aria-hidden="true" />
        <button
          class="type-menu-item flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-foreground transition-colors duration-200 hover:bg-muted"
          type="button"
          @click="handleOpenSettings"
        >
          <Settings class="h-3.5 w-3.5 text-primary/70" />
          <span>Settings</span>
        </button>
        <button
          class="type-menu-item flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-foreground transition-colors duration-200 hover:bg-muted"
          type="button"
          :aria-pressed="messageSoundsEnabled !== false"
          @click="handleToggleMessageSounds"
        >
          <Volume2 v-if="messageSoundsEnabled !== false" class="h-3.5 w-3.5 text-primary/70" />
          <VolumeX v-else class="h-3.5 w-3.5 text-muted-foreground" />
          <span>{{ messageSoundsEnabled !== false ? "Message sounds on" : "Message sounds off" }}</span>
        </button>
        <button
          class="type-menu-item flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-muted-foreground transition-colors duration-200 hover:bg-muted hover:text-destructive"
          type="button"
          @click="handleLogout"
        >
          <LogOut class="h-3.5 w-3.5" />
          <span>Log out</span>
        </button>
      </div>
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

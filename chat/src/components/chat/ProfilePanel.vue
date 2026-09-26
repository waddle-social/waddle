<script setup lang="ts">
import { computed, ref, useId, watch } from "vue";
import { Popover } from "@ark-ui/vue/popover";
import type { PopoverOpenChangeDetails, PopoverRootProps } from "@ark-ui/vue/popover";
import { Bell, BellOff, ChevronUp, LogOut, Settings, Volume2, VolumeX } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import PresencePicker from "@/components/chat/PresencePicker.vue";
import ThemeSwitcher from "@/components/chat/ThemeSwitcher.vue";
import VersionFooter from "@/components/chat/VersionFooter.vue";
import type { XmppServerVersion } from "@/shell/version";
import type { WaddleSession } from "@/lib/server-auth";

type PopoverPlacement = NonNullable<NonNullable<PopoverRootProps["positioning"]>["placement"]>;

const props = defineProps<{
  session: WaddleSession;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
  messageSoundsEnabled?: boolean;
  totalUnreadCount?: number;
  totalMentionCount?: number;
  compact?: boolean;
  /** Where the account popover opens relative to its trigger. Defaults
   * to below the avatar in compact (header) mode, above the account row
   * otherwise. */
  placement?: PopoverPlacement;
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

const accountMenuLabel = computed(() => `${props.session.username} — Account menu`);

// One open flag serves both layouts; the popover is re-created when
// `compact` flips, so close it rather than let it float in the wrong spot.
const menuOpen = ref(false);
const triggerId = useId();
const popoverIds = { trigger: triggerId };

const positioning = computed(() =>
  props.compact
    ? { placement: props.placement ?? ("bottom-end" as const), gutter: 8 }
    : { placement: props.placement ?? ("top-start" as const), gutter: 8 },
);

const popoverClass =
  "z-popover flex flex-col gap-1 rounded-lg border border-border bg-popover p-2 text-popover-foreground shadow-[var(--shadow-floating)] outline-none data-[state=open]:animate-fade-in";

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

function onOpenChange(details: PopoverOpenChangeDetails) {
  menuOpen.value = details.open;
}

function closeMenu() {
  menuOpen.value = false;
}

function handleOpenSettings() {
  closeMenu();
  emit("open-settings");
}

function handleLogout() {
  closeMenu();
  emit("logout");
}

watch(
  () => props.compact,
  () => closeMenu(),
);
</script>

<template>
  <!-- Compact: a header row (bell + avatar); the popover drops below. -->
  <div v-if="compact" class="relative flex items-center gap-1">
    <AppTooltip :label="bellTitle" placement="bottom">
      <button
        class="relative flex h-10 w-10 items-center justify-center rounded-lg transition-all duration-200"
        type="button"
        :class="notificationPermission === 'denied'
          ? 'cursor-not-allowed text-rail-foreground opacity-30'
          : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
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
          class="chat-badge-glow--mention type-count-badge absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full bg-live text-live-foreground"
          aria-hidden="true"
        >{{ totalMentionCount }}</span>
        <span
          v-else-if="(totalUnreadCount ?? 0) > 0"
          class="chat-badge-glow--primary type-count-badge absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full bg-primary text-primary-foreground"
          aria-hidden="true"
        >{{ totalUnreadCount }}</span>
      </button>
    </AppTooltip>
    <Popover.Root
      :open="menuOpen"
      :ids="popoverIds"
      :positioning="positioning"
      lazy-mount
      unmount-on-exit
      @open-change="onOpenChange"
    >
      <AppTooltip :label="accountMenuLabel" placement="bottom" :trigger-id="triggerId">
        <Popover.Trigger as-child>
          <button
            class="flex h-10 w-10 items-center justify-center rounded-lg text-rail-foreground transition-all duration-200 hover:bg-rail-hover hover:text-rail-active"
            type="button"
            :aria-label="accountMenuLabel"
          >
            <AppAvatar :name="session.username" :src="session.avatar_url" size="xs" />
          </button>
        </Popover.Trigger>
      </AppTooltip>
      <Popover.Positioner>
        <Popover.Content
          :class="[popoverClass, 'w-[var(--chat-profile-menu-width)]']"
          :aria-label="`${session.username} account menu`"
        >
          <div class="flex min-h-12 items-center gap-3 rounded-lg bg-muted/30 px-2.5 py-2">
            <AppAvatar :name="session.username" :src="session.avatar_url" size="sm" />
            <div class="min-w-0 flex-1">
              <Popover.Title as-child>
                <div class="type-menu-title truncate">{{ session.username }}</div>
              </Popover.Title>
              <div class="type-meta text-muted-foreground">Signed in</div>
            </div>
            <ThemeSwitcher />
          </div>
          <div class="mt-1 border-t border-border pt-2">
            <PresencePicker />
          </div>
          <div class="mt-1 flex flex-col gap-1 border-t border-border pt-2">
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
          <div class="mt-1 border-t border-border px-2.5 pt-2">
            <VersionFooter
              :web-commit-sha="webCommitSha"
              :server-version="serverVersion"
              layout="detail"
            />
          </div>
        </Popover.Content>
      </Popover.Positioner>
    </Popover.Root>
  </div>

  <div v-else class="flex flex-shrink-0 flex-col gap-2.5 border-t border-border px-3 py-2.5">
    <div class="relative flex items-center gap-2">
      <Popover.Root
        :open="menuOpen"
        :ids="popoverIds"
        :positioning="positioning"
        lazy-mount
        unmount-on-exit
        @open-change="onOpenChange"
      >
        <Popover.Trigger as-child>
          <button
            class="flex h-10 min-w-0 flex-1 items-center gap-2.5 rounded-lg px-1.5 text-left transition-all duration-200 hover:bg-sidebar-accent"
            type="button"
            :aria-label="accountMenuLabel"
          >
            <AppAvatar :name="session.username" :src="session.avatar_url" size="sm" />
            <span class="type-menu-title min-w-0 flex-1 truncate text-sidebar-foreground">{{ session.username }}</span>
            <ChevronUp
              class="h-3.5 w-3.5 flex-shrink-0 text-sidebar-muted transition-transform duration-200"
              :class="menuOpen ? 'rotate-180' : ''"
            />
          </button>
        </Popover.Trigger>
        <Popover.Positioner>
          <Popover.Content
            :class="[popoverClass, 'w-[var(--chat-account-menu-width)]']"
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
          </Popover.Content>
        </Popover.Positioner>
      </Popover.Root>
      <AppTooltip :label="bellTitle">
        <button
          class="relative flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-lg transition-all duration-200"
          type="button"
          :class="notificationPermission === 'denied'
            ? 'cursor-not-allowed opacity-30'
            : 'text-sidebar-muted hover:bg-sidebar-accent hover:text-primary'"
          :aria-label="bellAriaLabel"
          :disabled="notificationPermission === 'denied'"
          @click="handleBellClick"
        >
          <BellOff v-if="notificationPermission === 'denied' || (notificationPermission === 'granted' && !notificationsEnabled)" class="h-3.5 w-3.5" />
          <Bell v-else class="h-3.5 w-3.5" />
          <span
            v-if="(totalMentionCount ?? 0) > 0"
            class="type-count-badge absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full bg-live text-live-foreground"
            aria-hidden="true"
          >{{ totalMentionCount }}</span>
          <span
            v-else-if="(totalUnreadCount ?? 0) > 0"
            class="type-count-badge absolute -right-0.5 -top-0.5 inline-flex min-w-[14px] h-[14px] px-0.5 items-center justify-center rounded-full bg-primary text-primary-foreground"
            aria-hidden="true"
          >{{ totalUnreadCount }}</span>
        </button>
      </AppTooltip>
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

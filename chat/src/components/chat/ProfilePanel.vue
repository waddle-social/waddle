<script setup lang="ts">
import { computed, ref, useId, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Popover } from "@ark-ui/vue/popover";
import type { PopoverOpenChangeDetails, PopoverRootProps } from "@ark-ui/vue/popover";
import { Bell, BellOff, LogOut, Settings, SlidersHorizontal, Volume2, VolumeX } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import PresencePicker from "@/components/chat/PresencePicker.vue";
import ThemeSwitcher from "@/components/chat/ThemeSwitcher.vue";
import VersionFooter from "@/components/chat/VersionFooter.vue";
import { $selfPresenceShow } from "@/presence/presence-store";
import { menuClasses } from "@/ui/menu-classes";
import type { OccupantPresence } from "@/lib/xmpp-client";
import type { XmppServerVersion } from "@/shell/version";
import type { WaddleSession } from "@/lib/server-auth";

type PopoverPlacement = NonNullable<NonNullable<PopoverRootProps["positioning"]>["placement"]>;

const props = defineProps<{
  session: WaddleSession;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
  messageSoundsEnabled?: boolean;
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

const bellOff = computed(() =>
  props.notificationPermission === "denied" || (props.notificationPermission === "granted" && !props.notificationsEnabled),
);

const accountMenuLabel = computed(() => `${props.session.username} — Account menu`);

// The footer trigger reads the own status as others see it, auto-away
// included, so it matches the own avatar's dot (Sidebar board: "Available").
const selfShow = useStore($selfPresenceShow);
const STATUS: Record<string, { label: string; presence: OccupantPresence }> = {
  available: { label: "Available", presence: "online" },
  chat: { label: "Free to chat", presence: "online" },
  away: { label: "Away", presence: "away" },
  xa: { label: "Away", presence: "away" },
  dnd: { label: "Do Not Disturb", presence: "dnd" },
};
const status = computed(() => STATUS[selfShow.value] ?? STATUS.available!);

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

// Settings-Menu board.
const content = cx(menuClasses.content, css({ width: "300px", p: "2.5", gap: "1" }));
const header = css({ display: "flex", alignItems: "center", gap: "3", px: "1.5", pt: "1.5", pb: "2.5" });
const headerText = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", lineHeight: 1.3 });
const headerName = css({ font: "700 15px/1.3 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const caption = css({ textStyle: "caption", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const separator = css({ height: "1px", bg: "border.soft", mx: "1", my: "1" });
const row = cx(menuClasses.item, css({ _hover: { bg: "state.hover" } }));
const dangerRow = cx(row, css({ "&, & svg": { color: "danger.text" } }));
const version = css({ px: "3", pt: "1" });
// Footer trigger (Sidebar board): avatar + bold name + status caption.
const footer = css({ display: "flex", alignItems: "center", gap: "2.5", minWidth: 0, flex: 1 });
const footerButton = css({
  flex: 1,
  minWidth: 0,
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  height: "44px",
  pr: "1.5",
  pl: 0,
  border: 0,
  borderRadius: "control",
  bg: "transparent",
  color: "fg",
  textAlign: "left",
  cursor: "pointer",
  _hover: { bg: "state.hover" },
});
const footerText = css({ minWidth: 0, display: "flex", flexDirection: "column", "& b": { fontSize: "sm", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" } });
const compactRow = css({ position: "relative", display: "flex", alignItems: "center", gap: "1" });
const iconButton = iconbtn();
const bellButton = cx(iconButton, css({ _disabled: { opacity: 0.4, cursor: "not-allowed" } }));
</script>

<template>
  <div :class="compact ? compactRow : footer">
    <AppTooltip v-if="compact" :label="bellTitle" placement="bottom">
      <button
        :class="bellButton"
        type="button"
        :aria-label="bellTitle"
        :disabled="notificationPermission === 'denied'"
        @click="handleBellClick"
      >
        <BellOff v-if="bellOff" aria-hidden="true" />
        <Bell v-else aria-hidden="true" />
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
      <AppTooltip :label="compact ? accountMenuLabel : ''" placement="bottom" :trigger-id="triggerId">
        <Popover.Trigger as-child>
          <button v-if="compact" :class="iconButton" type="button" :aria-label="accountMenuLabel">
            <UserAvatar :name="session.username" :jid="session.jid" :fallback-src="session.avatar_url" size="sm" />
          </button>
          <button v-else :class="footerButton" type="button" :aria-label="accountMenuLabel">
            <UserAvatar :name="session.username" :jid="session.jid" :fallback-src="session.avatar_url" :presence="status.presence" />
            <span :class="footerText">
              <b>{{ session.username }}</b>
              <span :class="caption">{{ status.label }}</span>
            </span>
          </button>
        </Popover.Trigger>
      </AppTooltip>
      <Teleport to="body">
        <Popover.Positioner>
          <Popover.Content :class="content" :aria-label="`${session.username} account menu`">
            <div :class="header">
              <UserAvatar :name="session.username" :jid="session.jid" :fallback-src="session.avatar_url" :presence="status.presence" size="lg" />
              <div :class="headerText">
                <Popover.Title as-child>
                  <b :class="headerName">{{ session.username }}</b>
                </Popover.Title>
                <span :class="caption">{{ session.jid }}</span>
              </div>
            </div>
            <div :class="separator" aria-hidden="true" />
            <PresencePicker />
            <button
              :class="row"
              type="button"
              :disabled="notificationPermission === 'denied'"
              @click="handleBellClick"
            >
              <BellOff v-if="bellOff" aria-hidden="true" />
              <Bell v-else aria-hidden="true" />
              <span>{{ bellTitle }}</span>
            </button>
            <button
              :class="row"
              type="button"
              :aria-pressed="messageSoundsEnabled !== false"
              @click="handleToggleMessageSounds"
            >
              <Volume2 v-if="messageSoundsEnabled !== false" aria-hidden="true" />
              <VolumeX v-else aria-hidden="true" />
              <span>{{ messageSoundsEnabled !== false ? "Message sounds on" : "Message sounds off" }}</span>
            </button>
            <ThemeSwitcher />
            <button :class="row" type="button" @click="handleOpenSettings">
              <Settings aria-hidden="true" />
              <span>Settings</span>
            </button>
            <div :class="separator" aria-hidden="true" />
            <button :class="dangerRow" type="button" @click="handleLogout">
              <LogOut aria-hidden="true" />
              <span>Log out</span>
            </button>
            <div :class="version">
              <VersionFooter :web-commit-sha="webCommitSha" :server-version="serverVersion" layout="detail" />
            </div>
          </Popover.Content>
        </Popover.Positioner>
      </Teleport>
    </Popover.Root>
    <button v-if="!compact" :class="iconButton" type="button" aria-label="Settings" @click="emit('open-settings')">
      <SlidersHorizontal aria-hidden="true" />
    </button>
  </div>
</template>

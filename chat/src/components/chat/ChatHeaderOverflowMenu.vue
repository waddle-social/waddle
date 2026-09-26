<script setup lang="ts">
import { Menu } from "@ark-ui/vue/menu";
import { Ellipsis, Phone, Pin, Settings, Video } from "lucide-vue-next";
import AppMenu from "@/components/ui/AppMenu.vue";
import { menuClasses, menuItemIconClass } from "@/ui/menu-classes";

/**
 * Compact-layout overflow for the conversation header. Below `lg` the
 * header keeps only search and members inline; call starts, pins,
 * notifications and channel settings live here so the title keeps its
 * width. The default slot takes extra rows (the notification submenu).
 */
defineProps<{
  showCallItems: boolean;
  callBusy: boolean;
  pinnedOpen: boolean;
  showChannelSettings: boolean;
}>();

const emit = defineEmits<{
  voiceCall: [];
  videoCall: [];
  togglePinned: [];
  editChannel: [];
}>();

function onSelect(value: string) {
  switch (value) {
    case "voice-call":
      emit("voiceCall");
      return;
    case "video-call":
      emit("videoCall");
      return;
    case "pinned":
      emit("togglePinned");
      return;
    case "channel-settings":
      emit("editChannel");
      return;
  }
}
</script>

<template>
  <AppMenu
    tooltip="More actions"
    aria-label="Conversation actions"
    placement="bottom-end"
    content-class="w-60 max-w-[calc(100vw-1rem)]"
    @select="onSelect"
  >
    <template #trigger>
      <button
        class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground data-[state=open]:bg-muted data-[state=open]:text-foreground"
        type="button"
        aria-label="More actions"
      >
        <Ellipsis class="w-4 h-4" />
      </button>
    </template>
    <template v-if="showCallItems">
      <Menu.Item
        value="voice-call"
        :disabled="callBusy"
        :class="[menuClasses.item, 'data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50']"
      >
        <span :class="menuItemIconClass" aria-hidden="true"><Phone class="h-4 w-4" /></span>
        <Menu.ItemText>Voice call</Menu.ItemText>
      </Menu.Item>
      <Menu.Item
        value="video-call"
        :disabled="callBusy"
        :class="[menuClasses.item, 'data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50']"
      >
        <span :class="menuItemIconClass" aria-hidden="true"><Video class="h-4 w-4" /></span>
        <Menu.ItemText>Video call</Menu.ItemText>
      </Menu.Item>
      <Menu.Separator :class="menuClasses.separator" />
    </template>
    <Menu.Item value="pinned" :class="menuClasses.item">
      <span :class="menuItemIconClass" aria-hidden="true"><Pin class="h-4 w-4" /></span>
      <Menu.ItemText>{{ pinnedOpen ? "Hide pinned messages" : "Pinned messages" }}</Menu.ItemText>
    </Menu.Item>
    <slot />
    <Menu.Item v-if="showChannelSettings" value="channel-settings" :class="menuClasses.item">
      <span :class="menuItemIconClass" aria-hidden="true"><Settings class="h-4 w-4" /></span>
      <Menu.ItemText>Channel settings</Menu.ItemText>
    </Menu.Item>
  </AppMenu>
</template>

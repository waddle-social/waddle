<script setup lang="ts">
import { Menu } from "@ark-ui/vue/menu";
import { Ellipsis, Phone, Pin, Settings, Video } from "lucide-vue-next";
import AppMenu from "@/components/ui/AppMenu.vue";
import { iconbtn } from "styled-system/recipes";
import { menuClasses, menuItemIconClass } from "@/ui/menu-classes";

/**
 * Compact-layout overflow for the conversation header. Below `lg` the
 * header keeps only search and members inline; call starts, pins,
 * notifications and channel settings live here so the title keeps its
 * width. The default slot takes extra rows (the notification submenu).
 */
withDefaults(defineProps<{
  showCallItems: boolean;
  /** Rooms say huddle, direct messages say call. */
  callNoun?: "huddle" | "call";
  callBusy: boolean;
  pinnedOpen: boolean;
  showChannelSettings: boolean;
}>(), { callNoun: "call" });

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
    @select="onSelect"
  >
    <template #trigger>
      <button
        :class="iconbtn()"
        type="button"
        aria-label="More actions"
      >
        <Ellipsis aria-hidden="true" />
      </button>
    </template>
    <template v-if="showCallItems">
      <Menu.Item
        value="voice-call"
        :disabled="callBusy"
        :class="menuClasses.item"
      >
        <span :class="menuItemIconClass" aria-hidden="true"><Phone /></span>
        <Menu.ItemText>Voice {{ callNoun }}</Menu.ItemText>
      </Menu.Item>
      <Menu.Item
        value="video-call"
        :disabled="callBusy"
        :class="menuClasses.item"
      >
        <span :class="menuItemIconClass" aria-hidden="true"><Video /></span>
        <Menu.ItemText>Video {{ callNoun }}</Menu.ItemText>
      </Menu.Item>
      <Menu.Separator :class="menuClasses.separator" />
    </template>
    <Menu.Item value="pinned" :class="menuClasses.item">
      <span :class="menuItemIconClass" aria-hidden="true"><Pin /></span>
      <Menu.ItemText>{{ pinnedOpen ? "Hide pinned messages" : "Pinned messages" }}</Menu.ItemText>
    </Menu.Item>
    <slot />
    <Menu.Item v-if="showChannelSettings" value="channel-settings" :class="menuClasses.item">
      <span :class="menuItemIconClass" aria-hidden="true"><Settings /></span>
      <Menu.ItemText>Room settings</Menu.ItemText>
    </Menu.Item>
  </AppMenu>
</template>

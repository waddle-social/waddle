<script setup lang="ts">
/**
 * Prototype: an Ark UI Menu styled with the `menu` slot recipe from
 * docs/design/web-redesign/panda.config.ts. Shows the intended pattern for
 * replacing the six hand-rolled `role="menu"` implementations in chat/src.
 *
 * Imports point at the paths they would have once Panda is installed in the
 * chat workspace (`styled-system/` is Panda's generated output directory).
 */
import { Menu } from "@ark-ui/vue/menu";
import { MessageCircle, Mic, ShieldAlert, UserRound } from "lucide-vue-next";
import { menu } from "../styled-system/recipes";
import { avatar } from "../styled-system/recipes";

const props = defineProps<{
  name: string;
  initial: string;
  show: "available" | "chat" | "away" | "xa" | "dnd" | "offline";
}>();

const emit = defineEmits<{
  message: [];
  inviteToHuddle: [];
  viewProfile: [];
  report: [];
}>();

const classes = menu();
const avatarClasses = avatar({ size: "md" });

function onSelect(details: { value: string }) {
  switch (details.value) {
    case "message":
      return emit("message");
    case "huddle":
      return emit("inviteToHuddle");
    case "profile":
      return emit("viewProfile");
    case "report":
      return emit("report");
  }
}
</script>

<template>
  <Menu.Root :positioning="{ placement: 'bottom-start' }" @select="onSelect">
    <Menu.Trigger :class="classes.trigger" :aria-label="`Actions for ${props.name}`">
      <span :class="avatarClasses.root">
        <span :class="avatarClasses.fallback">{{ props.initial }}</span>
        <span :class="avatarClasses.presence" :data-show="props.show" />
      </span>
    </Menu.Trigger>
    <Menu.Positioner>
      <Menu.Content :class="classes.content">
        <Menu.ItemGroup>
          <Menu.ItemGroupLabel :class="classes.itemGroupLabel">{{ props.name }}</Menu.ItemGroupLabel>
          <Menu.Item value="message" :class="classes.item"><MessageCircle :size="16" />Message</Menu.Item>
          <Menu.Item value="huddle" :class="classes.item"><Mic :size="16" />Invite to huddle</Menu.Item>
          <Menu.Item value="profile" :class="classes.item"><UserRound :size="16" />View profile</Menu.Item>
        </Menu.ItemGroup>
        <Menu.Separator :class="classes.separator" />
        <Menu.Item value="report" :class="classes.item" data-tone="danger"><ShieldAlert :size="16" />Report</Menu.Item>
      </Menu.Content>
    </Menu.Positioner>
  </Menu.Root>
</template>

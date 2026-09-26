<script setup lang="ts">
/**
 * Prototype: an Ark UI Menu styled with the `menu` and `avatar` slot recipes
 * from docs/design/web-redesign/panda.config.ts. This is the pattern for
 * replacing the six hand-rolled `role="menu"` implementations in chat/src.
 *
 * People first: when the member is in a huddle, joining it is the first item.
 *
 * Imports point at the paths they would have once Panda is installed in the
 * chat workspace (`styled-system/` is Panda's generated output directory).
 */
import { Menu } from "@ark-ui/vue/menu";
import { Award, MessageCircle, Mic, ShieldAlert, UserRound } from "lucide-vue-next";
import { avatar, menu } from "../styled-system/recipes";

const props = defineProps<{
  name: string;
  initial: string;
  show: "available" | "chat" | "away" | "xa" | "dnd" | "offline";
  /** Name of the huddle the member is in, if any. */
  huddle?: string;
  speaking?: boolean;
}>();

const emit = defineEmits<{
  joinHuddle: [];
  message: [];
  giveKudos: [];
  viewProfile: [];
  report: [];
}>();

const classes = menu();
const avatarClasses = avatar({ size: "md", huddle: Boolean(props.huddle), speaking: Boolean(props.speaking) });

function onSelect(details: { value: string }) {
  switch (details.value) {
    case "huddle":
      return emit("joinHuddle");
    case "message":
      return emit("message");
    case "kudos":
      return emit("giveKudos");
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
          <Menu.ItemGroupLabel :class="classes.itemGroupLabel">
            {{ props.name }}<template v-if="props.huddle"> · in a huddle</template>
          </Menu.ItemGroupLabel>
          <Menu.Item v-if="props.huddle" value="huddle" :class="classes.item"><Mic :size="16" />Join {{ props.huddle }}</Menu.Item>
          <Menu.Item value="message" :class="classes.item"><MessageCircle :size="16" />Message</Menu.Item>
          <Menu.Item value="kudos" :class="classes.item"><Award :size="16" />Give kudos</Menu.Item>
          <Menu.Item value="profile" :class="classes.item"><UserRound :size="16" />View profile</Menu.Item>
        </Menu.ItemGroup>
        <Menu.Separator :class="classes.separator" />
        <Menu.Item value="report" :class="classes.item" data-tone="danger"><ShieldAlert :size="16" />Report</Menu.Item>
      </Menu.Content>
    </Menu.Positioner>
  </Menu.Root>
</template>

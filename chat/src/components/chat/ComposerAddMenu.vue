<script setup lang="ts">
import { computed, type Component } from "vue";
import { Menu } from "@ark-ui/vue/menu";
import type { MenuInteractOutsideEvent, MenuOpenChangeDetails, MenuSelectionDetails } from "@ark-ui/vue/menu";
import { ImagePlay, Puzzle, Upload } from "lucide-vue-next";
import { menuClasses, menuItemHintClass, menuItemIconClass, menuItemStackClass } from "@/ui/menu-classes";

/**
 * The composer's `+` menu: everything that adds content other than typed
 * text. Mirrors Slack's attach menu — upload, GIF search, and the
 * extensions launcher when the surface supports it.
 *
 * An Ark Menu anchored to the composer's `+` button (`anchorEl`), which
 * lives in `MessageComposer`; the composer mounts this component while
 * the menu is open and unmounts it on `close`.
 */
const props = defineProps<{
  anchorEl: HTMLElement | null;
  showExtensions: boolean;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  upload: [];
  gif: [];
  extensions: [];
  /** `outside` = dismissed by a pointer elsewhere, which already moves focus. */
  close: [reason: "escape" | "tab" | "outside"];
}>();

type AddMenuItem = { id: "upload" | "gif" | "extensions"; label: string; hint: string; icon: Component };

const items = computed<AddMenuItem[]>(() => [
  { id: "upload", label: "Upload from your computer", hint: "Images, videos, and files", icon: Upload },
  { id: "gif", label: "GIF", hint: "Search GIPHY", icon: ImagePlay },
  ...(props.showExtensions
    ? [{ id: "extensions", label: "Extensions", hint: "Run an app command", icon: Puzzle } as AddMenuItem]
    : []),
]);

const positioning = computed(() => ({
  placement: props.isTopPinned ? ("bottom-start" as const) : ("top-start" as const),
  gutter: 8,
  getAnchorRect: () => props.anchorEl?.getBoundingClientRect() ?? null,
}));

let closeReason: "escape" | "tab" | "outside" | null = null;

function onSelect(details: MenuSelectionDetails) {
  const id = details.value as AddMenuItem["id"];
  if (id === "upload") emit("upload");
  else if (id === "gif") emit("gif");
  else if (id === "extensions") emit("extensions");
}

function onEscapeKeyDown() {
  closeReason = "escape";
}

function onInteractOutside(event: MenuInteractOutsideEvent) {
  const target = event.detail.originalEvent.target as Node | null;
  // The `+` button toggles the menu itself; don't close-then-reopen.
  if (target && props.anchorEl?.contains(target)) {
    event.preventDefault();
    return;
  }
  closeReason = "outside";
}

function onOpenChange(details: MenuOpenChangeDetails) {
  if (details.open) return;
  const reason = closeReason ?? "tab";
  closeReason = null;
  emit("close", reason);
}

function onContentKeydown(event: KeyboardEvent) {
  if (event.key === "Tab") closeReason = "tab";
}
</script>

<template>
  <Menu.Root
    :open="true"
    :positioning="positioning"
    default-highlighted-value="upload"
    @select="onSelect"
    @escape-key-down="onEscapeKeyDown"
    @interact-outside="onInteractOutside"
    @open-change="onOpenChange"
  >
    <Menu.Positioner>
      <Menu.Content
        :class="[menuClasses.content, 'chat-composer-add-menu animate-fade-in']"
        aria-label="Add to message"
        @keydown="onContentKeydown"
      >
        <Menu.Item
          v-for="item in items"
          :key="item.id"
          :value="item.id"
          :class="[menuClasses.item, 'chat-composer-add-menu__item']"
          style="height: auto"
        >
          <span :class="menuItemIconClass" aria-hidden="true">
            <component :is="item.icon" class="h-4 w-4" />
          </span>
          <span :class="menuItemStackClass">
            <Menu.ItemText class="type-control text-foreground">{{ item.label }}</Menu.ItemText>
            <span :class="menuItemHintClass">{{ item.hint }}</span>
          </span>
        </Menu.Item>
      </Menu.Content>
    </Menu.Positioner>
  </Menu.Root>
</template>

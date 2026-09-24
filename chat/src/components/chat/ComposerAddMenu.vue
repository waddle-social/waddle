<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, type Component } from "vue";
import { ImagePlay, Puzzle, Upload } from "lucide-vue-next";

/**
 * The composer's `+` menu: everything that adds content other than typed
 * text. Mirrors Slack's attach menu — upload, GIF search, and the
 * extensions launcher when the surface supports it.
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

const menuEl = ref<HTMLElement | null>(null);

function itemButtons(): HTMLButtonElement[] {
  return Array.from(menuEl.value?.querySelectorAll<HTMLButtonElement>("[role='menuitem']") ?? []);
}

function pick(id: AddMenuItem["id"]) {
  if (id === "upload") emit("upload");
  else if (id === "gif") emit("gif");
  else emit("extensions");
}

function moveFocus(delta: number) {
  const buttons = itemButtons();
  if (buttons.length === 0) return;
  const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
  const next = (current + delta + buttons.length) % buttons.length;
  buttons[next]?.focus();
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "ArrowDown") {
    event.preventDefault();
    moveFocus(1);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    moveFocus(-1);
  } else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    emit("close", "escape");
  } else if (event.key === "Tab") {
    emit("close", "tab");
  }
}

function onWindowPointer(event: PointerEvent) {
  const target = event.target as Node | null;
  if (!target) return;
  if (menuEl.value?.contains(target)) return;
  if (props.anchorEl?.contains(target)) return;
  emit("close", "outside");
}

onMounted(() => {
  window.addEventListener("pointerdown", onWindowPointer, true);
  void nextTick(() => itemButtons()[0]?.focus());
});

onBeforeUnmount(() => {
  window.removeEventListener("pointerdown", onWindowPointer, true);
});
</script>

<template>
  <div
    ref="menuEl"
    role="menu"
    aria-label="Add to message"
    class="chat-composer-add-menu z-popover absolute left-0 bg-popover text-popover-foreground border border-border rounded-lg shadow-2xl animate-fade-in"
    :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
    @keydown="onKeydown"
  >
    <button
      v-for="item in items"
      :key="item.id"
      type="button"
      role="menuitem"
      class="chat-composer-add-menu__item"
      @click="pick(item.id)"
    >
      <span class="chat-composer-add-menu__icon" aria-hidden="true">
        <component :is="item.icon" class="h-4 w-4" />
      </span>
      <span class="flex min-w-0 flex-col text-left">
        <span class="type-control text-foreground">{{ item.label }}</span>
        <span class="type-caption text-muted-foreground">{{ item.hint }}</span>
      </span>
    </button>
  </div>
</template>

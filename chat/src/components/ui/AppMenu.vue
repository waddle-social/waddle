<script setup lang="ts">
import { computed, useId } from "vue";
import { Menu } from "@ark-ui/vue/menu";
import type { MenuOpenChangeDetails, MenuSelectionDetails } from "@ark-ui/vue/menu";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import { menuClasses } from "@/ui/menu-classes";

type MenuPlacement =
  | "top"
  | "top-start"
  | "top-end"
  | "bottom"
  | "bottom-start"
  | "bottom-end"
  | "left"
  | "left-start"
  | "left-end"
  | "right"
  | "right-start"
  | "right-end";

/**
 * Thin wrapper around Ark Menu: the `trigger` slot is the button that
 * opens it (Ark merges its trigger attributes onto it), the default slot
 * holds `Menu.Item` / `Menu.ItemGroup` / `Menu.RadioItemGroup` /
 * `Menu.CheckboxItem` / `Menu.Separator` parts rendered by the consumer,
 * styled with `menuClasses` from `@/ui/menu-classes`.
 *
 * Open state is uncontrolled unless `open` is bound (`v-model:open`).
 * `tooltip` adds an `AppTooltip` on the trigger that shares the trigger's
 * id with the menu. The content is portalled to `<body>` and lazily
 * mounted.
 *
 * `submenu` nests this menu inside a parent `AppMenu`'s content: the
 * `trigger` slot becomes a `Menu.TriggerItem` row of the parent, and Ark
 * links the two menus for keyboard navigation and dismissal.
 */
const props = withDefaults(
  defineProps<{
    open?: boolean;
    placement?: MenuPlacement;
    ariaLabel?: string;
    closeOnSelect?: boolean;
    tooltip?: string;
    submenu?: boolean;
    /** Extra classes for the content surface (width, etc.). */
    contentClass?: string;
  }>(),
  { open: undefined, placement: "bottom-end", ariaLabel: undefined, closeOnSelect: true, tooltip: "", submenu: false, contentClass: "" },
);

const emit = defineEmits<{
  "update:open": [open: boolean];
  select: [value: string];
}>();

const triggerId = useId();
const ids = { trigger: triggerId };
// Zag pins a submenu's placement beside its trigger row; `overlap` lets
// it slide over the parent menu instead of off a narrow viewport.
const positioning = computed(() => ({ placement: props.placement, gutter: 6, overlap: props.submenu }));

function onOpenChange(details: MenuOpenChangeDetails) {
  emit("update:open", details.open);
}

function onSelect(details: MenuSelectionDetails) {
  emit("select", details.value);
}
</script>

<template>
  <Menu.Root
    :open="open"
    :ids="submenu ? undefined : ids"
    :positioning="positioning"
    :close-on-select="closeOnSelect"
    lazy-mount
    unmount-on-exit
    @open-change="onOpenChange"
    @select="onSelect"
  >
    <Menu.TriggerItem v-if="submenu" as-child>
      <slot name="trigger" />
    </Menu.TriggerItem>
    <AppTooltip v-else :label="tooltip" :trigger-id="triggerId">
      <Menu.Trigger as-child>
        <slot name="trigger" />
      </Menu.Trigger>
    </AppTooltip>
    <Teleport to="body">
      <Menu.Positioner>
        <Menu.Content
          :class="[menuClasses.content, contentClass, 'data-[state=open]:animate-fade-in']"
          v-bind="ariaLabel ? { 'aria-label': ariaLabel } : {}"
        >
          <slot />
        </Menu.Content>
      </Menu.Positioner>
    </Teleport>
  </Menu.Root>
</template>

<script setup lang="ts">
import { Dialog } from "@ark-ui/vue/dialog";
import type { DialogOpenChangeDetails } from "@ark-ui/vue/dialog";
import { X } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { iconbtn, scrim } from "styled-system/recipes";

/**
 * Edge-anchored modal panel (Ark Dialog at the left or right edge), the
 * Mobile-Details board's drawer: rail ground, a title row with a 44px
 * close button, then the scrolling body. Keeps the `side` / `widthClass`
 * / `label` contract and the `title` + default slots. `bare` drops the
 * title row so the content (e.g. the sidebar) brings its own header.
 */
const open = defineModel<boolean>("open", { required: true });
const props = defineProps<{ side: "left" | "right"; widthClass?: string; label?: string; bare?: boolean }>();

const positionerClass = css({ position: "fixed", top: 0, bottom: 0, zIndex: "dialog", display: "flex" });
const contentClass = css({
  "--ring-bg": "{colors.bg.rail}",
  display: "flex",
  flexDirection: "column",
  height: "100dvh",
  maxWidth: "100vw",
  bg: "bg.rail",
  color: "fg",
  boxShadow: "overlay",
  outline: "none",
  animation: "wd-slide {durations.slow} {easings.out} backwards",
  "&[data-side=left]": { borderRight: "1px solid {colors.border}", "--wd-slide": "-16px" },
  "&[data-side=right]": { borderLeft: "1px solid {colors.border}" },
});
const defaultWidth = css({ width: "min(88vw, 320px)" });
const headClass = css({ display: "flex", alignItems: "center", gap: "2", flex: "none", padding: "{spacing.3} {spacing.3} {spacing.1} {spacing.4}" });
const titleClass = css({ flex: 1, minWidth: 0, font: "700 18px/1.3 {fonts.display}" });
const closeClass = cx(iconbtn(), css({ width: "44px", height: "44px" }));
const bodyClass = css({ flex: 1, minHeight: 0, overflowY: "auto", overscrollBehavior: "contain" });

function onOpenChange(details: DialogOpenChangeDetails) {
  open.value = details.open;
}
</script>

<template>
  <Dialog.Root :open="open" lazy-mount unmount-on-exit @open-change="onOpenChange">
    <Teleport to="body">
      <Dialog.Backdrop :class="scrim()" />
      <Dialog.Positioner :class="positionerClass" :style="side === 'left' ? { left: 0 } : { right: 0 }">
        <Dialog.Content
          :class="[contentClass, props.widthClass ?? defaultWidth]"
          :data-side="side"
          :aria-label="props.label ?? 'Drawer'"
        >
          <div v-if="!bare" :class="headClass">
            <div :class="titleClass"><slot name="title" /></div>
            <Dialog.CloseTrigger as-child>
              <button :class="closeClass" type="button" aria-label="Close drawer">
                <X aria-hidden="true" />
              </button>
            </Dialog.CloseTrigger>
          </div>
          <div :class="bodyClass">
            <slot />
          </div>
        </Dialog.Content>
      </Dialog.Positioner>
    </Teleport>
  </Dialog.Root>
</template>

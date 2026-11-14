<script setup lang="ts">
import { Menu } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";

const emit = defineEmits<{
  openNav: [];
}>();

// Mobile-Settings board: an ink (rail) bar in both themes, a 44px sidebar
// button and the page title. The sidebar is inline from md, so it hides there.
const barClass = css({
  position: "sticky",
  top: 0,
  zIndex: "sticky",
  display: "flex",
  alignItems: "center",
  gap: "1.5",
  padding: "2",
  borderBottom: "1px solid {colors.border.soft}",
  bg: "bg.rail",
  color: "fg",
  md: { display: "none" },
});
const buttonClass = cx(iconbtn(), css({ width: "44px", height: "44px" }));
const titleClass = css({ flex: 1, minWidth: 0, margin: 0, font: "700 18px/1.3 {fonts.display}" });
</script>

<template>
  <header :class="barClass" data-theme="night">
    <button :class="buttonClass" type="button" aria-label="Open navigation" @click="emit('openNav')">
      <Menu aria-hidden="true" />
    </button>
    <span :class="titleClass">Settings</span>
  </header>
</template>

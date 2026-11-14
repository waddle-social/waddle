<script setup lang="ts">
import { Toast, Toaster } from "@ark-ui/vue/toast";
import { X } from "lucide-vue-next";
import { css } from "styled-system/css";
import { toastClasses as cls } from "@/ui/part-classes";
import { toaster, type ToastMeta } from "@/ui/toaster";

/**
 * Renders the app-wide toast stack from `@/ui/toaster`. Mount it once,
 * near the root (XmppProvider). The surface is the preset `toast` recipe;
 * `live` toasts get the aurora edge, `danger` the krill one.
 */
const TONE_BORDER: Record<string, string> = {
  live: "var(--colors-live)",
  danger: "var(--colors-danger)",
  neutral: "var(--colors-elev-2)",
};
const textClass = css({ display: "flex", minWidth: 0, flex: 1, flexDirection: "column", gap: "1" });
const actionRowClass = css({ mt: "1.5" });

function borderFor(meta: Record<string, unknown> | undefined): string {
  const tone = (meta as Partial<ToastMeta> | undefined)?.tone;
  return TONE_BORDER[tone ?? "neutral"] ?? TONE_BORDER.neutral!;
}
</script>

<template>
  <Toaster :toaster="toaster" v-slot="item">
    <Toast.Root :class="[cls.root, 'app-toast']" :style="{ borderColor: borderFor(item.meta) }">
      <div :class="textClass">
        <Toast.Title :class="cls.title">{{ item.title }}</Toast.Title>
        <Toast.Description v-if="item.description" :class="cls.description">{{ item.description }}</Toast.Description>
        <div v-if="item.action" :class="actionRowClass">
          <Toast.ActionTrigger :class="cls.actionTrigger">{{ item.action.label }}</Toast.ActionTrigger>
        </div>
      </div>
      <Toast.CloseTrigger
        :class="cls.closeTrigger"
        aria-label="Dismiss"
      >
        <X aria-hidden="true" />
      </Toast.CloseTrigger>
    </Toast.Root>
  </Toaster>
</template>

<style>
/* Ark drives the stack through custom properties on each toast root;
 * these rules turn them into motion. */
.app-toast {
  translate: var(--x) var(--y);
  scale: var(--scale);
  z-index: var(--z-index);
  height: var(--height);
  opacity: var(--opacity);
  will-change: translate, opacity, scale;
  transition:
    translate 240ms cubic-bezier(0.2, 0.7, 0.2, 1),
    scale 240ms cubic-bezier(0.2, 0.7, 0.2, 1),
    opacity 240ms cubic-bezier(0.2, 0.7, 0.2, 1);
}

@media (prefers-reduced-motion: reduce) {
  .app-toast {
    transition: none;
  }
}
</style>

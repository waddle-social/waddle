<script setup lang="ts">
import { Toast, Toaster } from "@ark-ui/vue/toast";
import { X } from "lucide-vue-next";
import { toast as toastRecipe } from "styled-system/recipes";
import { toaster, type ToastMeta } from "@/ui/toaster";

/**
 * Renders the app-wide toast stack from `@/ui/toaster`. Mount it once,
 * near the root (XmppProvider). Surfaces come from the `toast` slot
 * recipe: opaque ink, hairline border, ember for `live`.
 */
const cls = toastRecipe();

const TONE_BORDER: Record<string, string> = {
  live: "var(--live)",
  danger: "var(--destructive)",
  neutral: "var(--border)",
};

function borderFor(meta: Record<string, unknown> | undefined): string {
  const tone = (meta as Partial<ToastMeta> | undefined)?.tone;
  return TONE_BORDER[tone ?? "neutral"] ?? TONE_BORDER.neutral!;
}
</script>

<template>
  <Toaster :toaster="toaster" v-slot="item">
    <Toast.Root :class="[cls.root, 'app-toast']" :style="{ borderColor: borderFor(item.meta) }">
      <div class="flex min-w-0 flex-1 flex-col gap-1">
        <Toast.Title :class="cls.title">{{ item.title }}</Toast.Title>
        <Toast.Description v-if="item.description" :class="cls.description">{{ item.description }}</Toast.Description>
        <div v-if="item.action" class="mt-1.5">
          <Toast.ActionTrigger :class="[cls.actionTrigger, 'inline-flex items-center']">{{ item.action.label }}</Toast.ActionTrigger>
        </div>
      </div>
      <Toast.CloseTrigger
        :class="[cls.closeTrigger, 'inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-md']"
        aria-label="Dismiss"
      >
        <X class="h-3.5 w-3.5" aria-hidden="true" />
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

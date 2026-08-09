<script setup lang="ts">
import { RefreshCw, WifiOff } from "lucide-vue-next";

/**
 * Superseded-session recovery banner for the non-conversation surfaces
 * (dashboard, feed, events, threads, unread, settings). The superseded
 * latch blocks every automatic reconnect, and `ContentArea`'s connection
 * banner only exists on the conversation surface — without this banner a
 * superseded tab parked on any sibling surface has no visible way to
 * invoke recovery short of navigating away or reloading.
 *
 * Mirrors the offline-tone connection banner styling from `ContentArea`.
 */
defineProps<{
  detail?: string;
}>();

const emit = defineEmits<{
  recover: [];
}>();
</script>

<template>
  <div
    role="status"
    aria-live="polite"
    aria-atomic="true"
    class="border-b border-border/80 animate-fade-in bg-muted/35 text-foreground"
  >
    <div class="chat-message-lane flex flex-col gap-3 px-[var(--chat-content-inline)] py-3 sm:flex-row sm:items-center sm:justify-between">
      <div class="flex min-w-0 items-start gap-3">
        <div class="mt-0.5 flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full border border-border/70 bg-background/60 text-muted-foreground/80">
          <WifiOff class="h-4 w-4" />
        </div>
        <div class="flex min-w-0 flex-col gap-0.5">
          <p class="type-control">
            Session resumed in another tab
          </p>
          <p class="type-caption chat-copy-measure text-muted-foreground">
            {{ (detail ?? "").trim() || "This session was resumed in another tab." }} Reconnect to continue from this tab.
          </p>
        </div>
      </div>
      <button
        type="button"
        class="type-control inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-full bg-primary px-3.5 text-primary-foreground transition-colors hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/35"
        @click="emit('recover')"
      >
        <RefreshCw class="h-3.5 w-3.5" />
        <span>Reconnect</span>
      </button>
    </div>
  </div>
</template>

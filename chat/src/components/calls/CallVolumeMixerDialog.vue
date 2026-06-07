<script setup lang="ts">
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import CallVolumeMixerPanel from "./CallVolumeMixerPanel.vue";
import type { CallVolumeMixerRow } from "@/lib/calls/call-volume-mixer";

/**
 * Modal wrapper that surfaces the call volume mixer from the control
 * bar's speaker button (#909). The mixer's row model, sliders, and
 * reset-all are reused unchanged from `CallVolumeMixerPanel`; this
 * component only owns the open/close chrome.
 *
 * Lives in `<AppDialog>` for the same reason `CallSettingsDialog`
 * does: shared backdrop blur, Esc-to-close, and focus model. The
 * panel's own "Who you hear" header serves as the dialog title, so
 * the chrome adds only an explicit close affordance. A scoped
 * `:deep` override lets the sidebar-shaped panel fill the modal
 * without forking the panel component.
 */
const open = defineModel<boolean>("open", { required: true });

defineProps<{
  rows: readonly CallVolumeMixerRow[];
}>();

const emit = defineEmits<{
  setVolume: [row: CallVolumeMixerRow, level: number];
  resetAll: [];
}>();

function onSetVolume(row: CallVolumeMixerRow, level: number): void {
  emit("setVolume", row, level);
}

function onResetAll(): void {
  emit("resetAll");
}

function close(): void {
  open.value = false;
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="call-volume-mixer-dialog">
      <button
        type="button"
        class="call-volume-mixer-dialog__close chat-icon-button chat-icon-button--md hover:bg-muted"
        aria-label="Close volume mixer"
        @click="close"
      >
        <X class="w-4 h-4" />
      </button>
      <CallVolumeMixerPanel
        :rows="rows"
        @set-volume="onSetVolume"
        @reset-all="onResetAll"
      />
    </div>
  </AppDialog>
</template>

<style scoped>
.call-volume-mixer-dialog {
  position: relative;
  display: flex;
  flex-direction: column;
  min-height: 0;
  max-height: min(34rem, calc(100dvh - 6rem));
}

.call-volume-mixer-dialog__close {
  position: absolute;
  top: 0.5rem;
  right: 0.5rem;
  z-index: 1;
}

/* The mixer panel is shaped as an inline sidebar (`border-left`,
 * clamped width). Inside the modal it should fill the dialog instead;
 * override its container layout here rather than forking the shared
 * panel component. */
.call-volume-mixer-dialog :deep(.call-volume-mixer) {
  width: 100%;
  min-width: 0;
  border-left: 0;
  background: transparent;
}

.call-volume-mixer-dialog :deep(.call-volume-mixer__header) {
  padding-right: 3rem;
}
</style>

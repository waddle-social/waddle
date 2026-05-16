<script setup lang="ts">
import { ref, watch } from "vue";
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { ChannelEditFormData } from "@/lib/chat-ui";
import type { ChannelSummary, PinPermission, SpaceSummary } from "@/lib/chat-types";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  channel: ChannelSummary | null;
  form: ChannelEditFormData;
  isSubmitting: boolean;
  /** Spaces the user can move the channel into. */
  spaces: SpaceSummary[];
  /** Whether the user has permission to move the channel between
   * spaces. Falls back to the same `canManageChannels` gate as
   * editing other channel attributes. */
  canMoveChannel: boolean;
}>();

const emit = defineEmits<{
  save: [];
  delete: [];
  "update:form": [form: ChannelEditFormData];
  /** Move the current channel to a different Space. Wired to the
   * `moveChannelToSpace` waddle action. Distinct from `save` because
   * the move is a separate XMPP publish (not part of MUC owner
   * config), and we want the user to see immediate Move feedback
   * without the rest of the dialog gating on it. */
  moveToSpace: [targetSpaceId: string];
}>();

// Pending target Space for the "Move" subsection. Resets to the
// channel's current Space whenever the dialog is (re)opened, so the
// user starts each session at "no pending change."
const pendingTargetSpaceId = ref<string | null>(null);
watch(
  () => [open.value, props.channel?.id, props.channel?.spaceId] as const,
  () => {
    pendingTargetSpaceId.value = props.channel?.spaceId ?? null;
  },
  { immediate: true },
);

function onMoveClick() {
  const target = pendingTargetSpaceId.value;
  if (!target) return;
  if (target === (props.channel?.spaceId ?? null)) return;
  emit("moveToSpace", target);
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title truncate">
        {{ channel ? `Edit #${channel.name}` : "Edit channel" }}
      </h2>
      <button
        class="chat-icon-button flex-none hover:bg-muted"
        type="button"
        aria-label="Close edit channel dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-dialog-stack chat-dialog-body">
      <div class="chat-field-stack">
        <label for="edit-channel-name" class="type-section-label text-muted-foreground">
          Name
        </label>
        <input
          id="edit-channel-name"
          :value="form.name"
          class="chat-field-control type-field"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label for="edit-channel-description" class="type-section-label text-muted-foreground">
          Description
        </label>
        <textarea
          id="edit-channel-description"
          :value="form.description"
          class="chat-field-control chat-textarea-control type-field"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label for="edit-channel-pin-permission" class="type-section-label text-muted-foreground">
          Who can pin messages
        </label>
        <select
          id="edit-channel-pin-permission"
          :value="form.pinPermission ?? ''"
          class="chat-field-control type-field"
          @change="$emit('update:form', { ...form, pinPermission: ($event.target as HTMLSelectElement).value as PinPermission })"
        >
          <option value="" disabled>Keep current setting</option>
          <option value="admins-only">Admins only</option>
          <option value="anyone">Anyone</option>
        </select>
      </div>

      <!-- XEP-0503 Space membership. Distinct from name/description
           because the server-side change goes through a PubSub
           publish against the Spaces service (not the MUC owner
           form), so we surface its own Move button rather than
           folding it into the dialog-wide Save. The server enforces
           single-membership per #620: publishing to the target
           Space retracts the bookmark from any prior Space, so the
           client just issues the publish. -->
      <div v-if="canMoveChannel" class="chat-field-stack">
        <label for="edit-channel-space" class="type-section-label text-muted-foreground">
          Space
        </label>
        <div class="flex items-center gap-2">
          <select
            id="edit-channel-space"
            :value="pendingTargetSpaceId ?? ''"
            class="chat-field-control type-field flex-1"
            :disabled="isSubmitting || spaces.length === 0"
            @change="pendingTargetSpaceId = (($event.target as HTMLSelectElement).value || null)"
          >
            <option v-if="spaces.length === 0" value="" disabled>No other spaces</option>
            <option v-for="space in spaces" :key="space.id" :value="space.id">
              {{ space.name }}
            </option>
          </select>
          <button
            class="chat-action-button chat-action-button--secondary type-control"
            type="button"
            :disabled="
              isSubmitting
                || !pendingTargetSpaceId
                || pendingTargetSpaceId === (channel?.spaceId ?? null)
            "
            @click="onMoveClick"
          >
            {{ isSubmitting ? "Moving…" : "Move" }}
          </button>
        </div>
        <p
          v-if="channel?.spaceId && pendingTargetSpaceId && pendingTargetSpaceId !== channel.spaceId"
          class="type-meta text-muted-foreground"
        >
          The room will move from
          <span class="type-emphasis">
            {{ spaces.find((s) => s.id === channel?.spaceId)?.name ?? channel.spaceId }}
          </span>
          to
          <span class="type-emphasis">
            {{ spaces.find((s) => s.id === pendingTargetSpaceId)?.name ?? pendingTargetSpaceId }}
          </span>.
        </p>
      </div>
    </div>

    <div class="chat-dialog-footer flex-col sm:flex-row sm:items-center sm:justify-between">
      <button
        class="chat-action-button type-control text-left text-destructive hover:bg-destructive/10 sm:text-center"
        type="button"
        @click="emit('delete')"
      >
        Delete channel
      </button>
      <div class="flex justify-end gap-2">
        <button
          class="chat-action-button chat-action-button--secondary type-control flex-1 sm:flex-none"
          type="button"
          @click="open = false"
        >
          Cancel
        </button>
        <button
          class="chat-action-button chat-action-button--primary type-control flex-1 disabled:opacity-40 sm:flex-none"
          type="button"
          :disabled="isSubmitting || !form.name.trim()"
          @click="emit('save')"
        >
          {{ isSubmitting ? "Saving…" : "Save" }}
        </button>
      </div>
    </div>
  </AppDialog>
</template>

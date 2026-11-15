<script setup lang="ts">
import AppSelect from "@/components/ui/AppSelect.vue";
import { ref, watch } from "vue";
import { X } from "lucide-vue-next";
import { btn, iconbtn } from "styled-system/recipes";
import { css } from "styled-system/css";
import AppDialog from "@/components/ui/AppDialog.vue";
import { dialogParts } from "@/ui/part-classes";
import { formClasses as f } from "@/components/modals/form-classes";
import type { ChannelEditFormData } from "@/lib/chat-ui";
import type { ChannelSummary, PinPermission, SpaceSummary } from "@/lib/chat-types";

const PIN_PERMISSION_OPTIONS: Array<{ value: PinPermission; label: string }> = [
  { value: "admins-only", label: "Admins only" },
  { value: "anyone", label: "Anyone" },
];

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
const titleClass = css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const moveRow = css({ display: "flex", alignItems: "center", gap: "2", "& select": { flex: 1, minWidth: 0 } });
const strong = css({ color: "fg", fontWeight: 700 });
</script>

<template>
  <AppDialog v-model:open="open">
    <div :class="dialogParts.head">
      <h2 :class="titleClass">{{ channel ? `Edit ${channel.name}` : "Edit room" }}</h2>
      <button :class="iconbtn()" type="button" aria-label="Close edit channel dialog" @click="open = false">
        <X aria-hidden="true" />
      </button>
    </div>

    <div :class="dialogParts.body">
      <div :class="f.field">
        <label for="edit-channel-name" :class="f.label">Name</label>
        <input
          id="edit-channel-name"
          :value="form.name"
          :class="f.input"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div :class="f.field">
        <label for="edit-channel-description" :class="f.label">Description</label>
        <textarea
          id="edit-channel-description"
          :value="form.description"
          :class="f.textarea"
          rows="2"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div :class="f.field">
        <label for="edit-channel-pin-permission" :class="f.label">Who can pin messages</label>
        <AppSelect
          id="edit-channel-pin-permission"
          :model-value="form.pinPermission ?? ''"
          :options="PIN_PERMISSION_OPTIONS"
          placeholder="Keep current setting"
          :class="f.input"
          @update:model-value="$emit('update:form', { ...form, pinPermission: $event })"
        />
      </div>

      <!-- XEP-0503 Space membership. Distinct from name/description
           because the server-side change goes through a PubSub
           publish against the Spaces service (not the MUC owner
           form), so we surface its own Move button rather than
           folding it into the dialog-wide Save. The server enforces
           single-membership per #620: publishing to the target
           Space retracts the bookmark from any prior Space, so the
           client just issues the publish. -->
      <div v-if="canMoveChannel" :class="f.field">
        <label for="edit-channel-space" :class="f.label">Space</label>
        <div :class="moveRow">
          <AppSelect
            id="edit-channel-space"
            :model-value="pendingTargetSpaceId ?? ''"
            :options="spaces.map((space) => ({ value: space.id, label: space.name }))"
            placeholder="No other spaces"
            :class="f.input"
            :disabled="isSubmitting || spaces.length === 0"
            @update:model-value="pendingTargetSpaceId = $event || null"
          />
          <button
            :class="btn()"
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
          :class="f.caption"
        >
          The room will move from
          <span :class="strong">
            {{ spaces.find((s) => s.id === channel?.spaceId)?.name ?? channel.spaceId }}
          </span>
          to
          <span :class="strong">
            {{ spaces.find((s) => s.id === pendingTargetSpaceId)?.name ?? pendingTargetSpaceId }}
          </span>.
        </p>
      </div>
    </div>

    <div :class="[dialogParts.foot, f.footSplit]">
      <button :class="f.dangerOutline" type="button" @click="emit('delete')">Delete room</button>
      <button :class="btn()" type="button" @click="open = false">Cancel</button>
      <button
        :class="btn({ variant: 'primary' })"
        type="button"
        :disabled="isSubmitting || !form.name.trim()"
        @click="emit('save')"
      >
        {{ isSubmitting ? "Saving…" : "Save" }}
      </button>
    </div>
  </AppDialog>
</template>

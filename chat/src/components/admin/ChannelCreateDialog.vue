<script setup lang="ts">
import AppSelect from "@/components/ui/AppSelect.vue";
// Admin V2 — modal for `adminChannelsCreate`.
import { ref, watch } from "vue";
import { Dialog } from "@ark-ui/vue/dialog";
import { X } from "lucide-vue-next";
import { btn, iconbtn } from "styled-system/recipes";
import { adminClasses as a } from "@/components/admin/admin-classes";
import { dialogParts } from "@/ui/part-classes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { requireMembershipForUnlistedChannel } from "./channelAdmissionPolicy";
import type { WasmAdminChannelType } from "@/lib/xmpp";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  isSubmitting?: boolean;
  spaces?: { space_jid: string; space_node: string; name: string }[];
}>();

const emit = defineEmits<{
  submit: [payload: { name: string; topic?: string | null; channelType?: WasmAdminChannelType | null; spaceJid?: string | null; spaceNode?: string | null; isPublic?: boolean | null; membersOnly?: boolean | null }];
}>();

const name = ref("");
const topic = ref("");
const CHANNEL_TYPE_OPTIONS: Array<{ value: WasmAdminChannelType; label: string }> = [
  { value: "text", label: "Text" },
  { value: "announcement", label: "Announcement" },
  { value: "forum", label: "Forum" },
];
const channelType = ref<WasmAdminChannelType>("text");
const spaceNode = ref("");
const isPublic = ref(true);
const membersOnly = ref(false);

watch(open, (isOpen) => {
  if (isOpen) {
    name.value = "";
    topic.value = "";
    channelType.value = "text";
    spaceNode.value = "";
    isPublic.value = true;
    membersOnly.value = false;
  }
});

function handlePublicToggleChange() {
  const policy = requireMembershipForUnlistedChannel({
    isPublic: isPublic.value,
    membersOnly: membersOnly.value,
  });
  isPublic.value = policy.isPublic;
  membersOnly.value = policy.membersOnly;
}

function onSubmit() {
  if (!name.value.trim() || props.isSubmitting) return;
  const selectedSpace = props.spaces?.find((space) => space.space_node === spaceNode.value);
  emit("submit", {
    name: name.value.trim(),
    topic: topic.value.trim() || null,
    channelType: channelType.value,
    spaceJid: selectedSpace?.space_jid ?? null,
    spaceNode: selectedSpace?.space_node ?? null,
    isPublic: isPublic.value,
    membersOnly: membersOnly.value,
  });
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div :class="dialogParts.head">
      <Dialog.Title as-child><h2>New channel</h2></Dialog.Title>
      <button
        :class="iconbtn({ size: 'sm' })"
        type="button"
        aria-label="Close create-channel dialog"
        @click="open = false"
      >
        <X aria-hidden="true" />
      </button>
    </div>

    <form :class="dialogParts.body" @submit.prevent="onSubmit">
      <label :class="a.field">
        Name
        <input
          v-model="name"
          :class="a.input"
          type="text"
          required
          maxlength="80"
          autocomplete="off"
          placeholder="general"
        />
      </label>
      <label :class="a.field">
        Topic (optional)
        <textarea
          v-model="topic"
          :class="a.textarea"
          rows="2"
          placeholder="What's this channel for?"
        />
      </label>
      <div :class="a.grid2">
        <label v-if="spaces && spaces.length > 0" :class="a.field">
          Space (optional)
          <AppSelect
            v-model="spaceNode"
            :options="[{ value: '', label: 'No space' }, ...spaces.map((s) => ({ value: s.space_node, label: s.name }))]"
            :class="a.input"
          />
        </label>
        <label :class="a.field">
          Type
          <AppSelect v-model="channelType" :options="CHANNEL_TYPE_OPTIONS" :class="a.input" />
        </label>
      </div>
      <div :class="a.checks">
        <label>
          <input v-model="isPublic" type="checkbox" @change="handlePublicToggleChange" />
          Listed in discovery
        </label>
        <label>
          <input v-model="membersOnly" type="checkbox" />
          Require explicit membership
        </label>
        <p>Discovery visibility and room admission are separate XMPP room settings.</p>
      </div>
    </form>

    <div :class="dialogParts.foot">
      <button type="button" :class="btn()" :disabled="isSubmitting" @click="open = false">
        Cancel
      </button>
      <button
        type="button"
        :class="btn({ variant: 'primary' })"
        :disabled="!name.trim() || isSubmitting"
        @click="onSubmit"
      >
        {{ isSubmitting ? "Creating…" : "Create channel" }}
      </button>
    </div>
  </AppDialog>
</template>

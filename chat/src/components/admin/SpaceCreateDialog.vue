<script setup lang="ts">
// Admin V2 — modal for `adminSpacesCreate`. Mirrors the dialog
// surface used elsewhere in chat (AppDialog + `dialogParts`).
import { ref, watch } from "vue";
import { Dialog } from "@ark-ui/vue/dialog";
import { X } from "lucide-vue-next";
import { btn, iconbtn } from "styled-system/recipes";
import { adminClasses as a } from "@/components/admin/admin-classes";
import { dialogParts } from "@/ui/part-classes";
import AppDialog from "@/components/ui/AppDialog.vue";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  isSubmitting?: boolean;
}>();

const emit = defineEmits<{
  /** Caller invokes `adminSpacesCreate` with these args and resolves. */
  submit: [payload: { name: string; description?: string | null; iconUrl?: string | null }];
}>();

const name = ref("");
const description = ref("");
const iconUrl = ref("");

watch(open, (isOpen) => {
  if (isOpen) {
    name.value = "";
    description.value = "";
    iconUrl.value = "";
  }
});

function onSubmit() {
  if (!name.value.trim() || props.isSubmitting) return;
  emit("submit", {
    name: name.value.trim(),
    description: description.value.trim() || null,
    iconUrl: iconUrl.value.trim() || null,
  });
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div :class="dialogParts.head">
      <Dialog.Title as-child><h2>New space</h2></Dialog.Title>
      <button
        :class="iconbtn({ size: 'sm' })"
        type="button"
        aria-label="Close create-space dialog"
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
          placeholder="Engineering"
        />
      </label>
      <label :class="a.field">
        Description (optional)
        <textarea
          v-model="description"
          :class="a.textarea"
          rows="3"
          placeholder="What's this space for?"
        />
      </label>
      <label :class="a.field">
        Icon URL (optional)
        <input
          v-model="iconUrl"
          :class="a.input"
          type="url"
          autocomplete="off"
          placeholder="https://…"
        />
      </label>
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
        {{ isSubmitting ? "Creating…" : "Create space" }}
      </button>
    </div>
  </AppDialog>
</template>

<script setup lang="ts">
import { Lock, Globe, X } from "lucide-vue-next";
import { btn, iconbtn, option } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { dialogParts } from "@/ui/part-classes";
import { formClasses as f } from "@/components/modals/form-classes";
import type { CommunityFormData } from "@/lib/chat-ui";
import type { SpaceSummary } from "@/lib/chat-types";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  waddle: SpaceSummary | null;
  form: CommunityFormData;
  isSubmitting: boolean;
}>();

const emit = defineEmits<{
  save: [];
  delete: [];
  "update:form": [form: CommunityFormData];
}>();
</script>

<template>
  <AppDialog v-model:open="open">
    <div :class="dialogParts.head">
      <h2>Community settings</h2>
      <button :class="iconbtn()" type="button" aria-label="Close waddle settings dialog" @click="open = false">
        <X aria-hidden="true" />
      </button>
    </div>

    <div :class="dialogParts.body">
      <div :class="f.field">
        <label for="waddle-settings-name" :class="f.label">Name</label>
        <input
          id="waddle-settings-name"
          :value="form.name"
          :class="f.input"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div :class="f.field">
        <label for="waddle-settings-description" :class="f.label">Description</label>
        <textarea
          id="waddle-settings-description"
          :value="form.description"
          :class="f.textarea"
          rows="2"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div :class="f.field" role="group" aria-labelledby="waddle-settings-privacy">
        <span id="waddle-settings-privacy" :class="f.label">Privacy</span>
        <div :class="f.options">
          <button
            type="button"
            :class="option()"
            :aria-pressed="!form.is_public"
            @click="$emit('update:form', { ...form, is_public: false })"
          >
            <span :class="f.optionTitle"><Lock aria-hidden="true" />Private</span>
          </button>
          <button
            type="button"
            :class="option()"
            :aria-pressed="form.is_public"
            @click="$emit('update:form', { ...form, is_public: true })"
          >
            <span :class="f.optionTitle"><Globe aria-hidden="true" />Public</span>
          </button>
        </div>
      </div>
    </div>

    <div :class="[dialogParts.foot, f.footSplit]">
      <button :class="f.dangerOutline" type="button" @click="emit('delete')">Delete community</button>
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

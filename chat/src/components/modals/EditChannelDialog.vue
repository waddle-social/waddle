<script setup lang="ts">
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { ChannelEditFormData } from "@/lib/chat-ui";
import type { ChannelSummary } from "@/lib/waddle-api";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  channel: ChannelSummary | null;
  form: ChannelEditFormData;
  isSubmitting: boolean;
}>();

const emit = defineEmits<{
  save: [];
  delete: [];
  "update:form": [form: ChannelEditFormData];
}>();
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="flex h-14 items-center justify-between border-b border-border px-4 sm:px-5">
      <h2 class="text-[16px] font-display font-bold">
        Edit #{{ channel?.name }}
      </h2>
      <button class="flex h-8 w-8 items-center justify-center rounded-lg hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="space-y-4 px-4 py-4 sm:px-5">
      <div>
        <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
          Name
        </label>
        <input
          :value="form.name"
          class="h-9 w-full rounded-lg bg-muted px-3 text-[13px] transition-all duration-200 focus:outline-none focus:ring-2 focus:ring-primary/20"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div>
        <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
          Description
        </label>
        <textarea
          :value="form.description"
          class="min-h-20 w-full rounded-lg bg-muted px-3 py-2 text-[13px] transition-all duration-200 resize-y focus:outline-none focus:ring-2 focus:ring-primary/20"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>
    </div>

    <div class="flex min-h-14 items-center justify-between gap-3 border-t border-border px-4 py-3 sm:px-5">
      <button
        class="h-9 rounded-lg px-3 text-[13px] font-medium text-destructive hover:bg-destructive/10 transition-all duration-200"
        @click="emit('delete')"
      >
        Delete Channel
      </button>
      <div class="flex gap-2">
        <button
          class="h-9 rounded-lg border border-border px-3 text-[13px] font-medium hover:bg-muted transition-all duration-200"
          @click="open = false"
        >
          Cancel
        </button>
        <button
          class="h-9 rounded-lg bg-primary px-3 text-[13px] font-medium text-primary-foreground hover:shadow-[0_0_12px_var(--glow)] transition-all duration-200 disabled:opacity-40"
          :disabled="isSubmitting || !form.name.trim()"
          @click="emit('save')"
        >
          {{ isSubmitting ? "Saving..." : "Save" }}
        </button>
      </div>
    </div>
  </AppDialog>
</template>

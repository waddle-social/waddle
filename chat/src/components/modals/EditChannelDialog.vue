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
    <div class="border-b border-foreground p-6 flex items-center justify-between">
      <h2 class="text-xl font-mono font-bold uppercase tracking-wider">
        Edit #{{ channel?.name }}
      </h2>
      <button class="p-1 hover:bg-muted transition-colors" @click="open = false">
        <X class="w-5 h-5" />
      </button>
    </div>

    <div class="p-6 space-y-4">
      <div>
        <label class="block text-xs font-mono uppercase tracking-widest text-muted-foreground mb-2">
          Name
        </label>
        <input
          :value="form.name"
          class="w-full font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground px-3 py-2 bg-background text-sm"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div>
        <label class="block text-xs font-mono uppercase tracking-widest text-muted-foreground mb-2">
          Description
        </label>
        <textarea
          :value="form.description"
          class="w-full font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground px-3 py-2 bg-background text-sm min-h-20 resize-y"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>
    </div>

    <div class="border-t border-foreground p-6 flex items-center justify-between">
      <button
        class="font-mono uppercase tracking-wider text-sm py-2 px-4 border border-destructive text-destructive hover:bg-destructive hover:text-destructive-foreground transition-colors"
        @click="emit('delete')"
      >
        Delete Channel
      </button>
      <div class="flex gap-3">
        <button
          class="font-mono uppercase tracking-wider text-sm py-2 px-4 border border-foreground hover:bg-muted transition-colors"
          @click="open = false"
        >
          Cancel
        </button>
        <button
          class="font-mono uppercase tracking-wider text-sm py-2 px-4 bg-foreground text-background border border-foreground hover:bg-foreground/90 transition-colors disabled:opacity-50"
          :disabled="isSubmitting || !form.name.trim()"
          @click="emit('save')"
        >
          {{ isSubmitting ? "Saving..." : "Save" }}
        </button>
      </div>
    </div>
  </AppDialog>
</template>

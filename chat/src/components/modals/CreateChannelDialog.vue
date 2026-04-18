<script setup lang="ts">
import { Hash, MessagesSquare, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { ChannelCreateFormData } from "@/lib/chat-ui";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  form: ChannelCreateFormData;
  isSubmitting: boolean;
}>();

const emit = defineEmits<{
  submit: [];
  "update:form": [form: ChannelCreateFormData];
}>();
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="border-b border-border px-6 py-4 flex items-center justify-between">
      <h2 class="text-[16px] font-display font-bold">Create Channel</h2>
      <button class="p-1 rounded-xl hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="px-6 py-4 space-y-4">
      <div>
        <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
          Name
        </label>
        <input
          :value="form.name"
          class="w-full rounded-xl bg-muted focus:outline-none focus:ring-2 focus:ring-primary/20 px-3 py-2 text-[13px] transition-all duration-200"
          placeholder="general"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div>
        <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
          Description
        </label>
        <textarea
          :value="form.description"
          class="w-full rounded-xl bg-muted focus:outline-none focus:ring-2 focus:ring-primary/20 px-3 py-2 text-[13px] min-h-16 resize-y transition-all duration-200"
          placeholder="What is this channel for?"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div>
        <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
          Channel type
        </label>
        <div class="grid grid-cols-2 gap-2">
          <button
            type="button"
            class="rounded-xl border px-3 py-3 text-left transition-all duration-200"
            :class="form.channel_type === 'text'
              ? 'border-primary/30 bg-primary/8 shadow-[0_0_12px_var(--glow)]'
              : 'border-border bg-muted/40 hover:bg-muted'"
            @click="$emit('update:form', { ...form, channel_type: 'text' })"
          >
            <div class="flex items-center gap-2 text-[13px] font-medium">
              <Hash class="w-4 h-4 text-primary/80" />
              Text
            </div>
            <p class="mt-1 text-[11px] text-muted-foreground">
              Fast back-and-forth conversation.
            </p>
          </button>
          <button
            type="button"
            class="rounded-xl border px-3 py-3 text-left transition-all duration-200"
            :class="form.channel_type === 'forum'
              ? 'border-primary/30 bg-primary/8 shadow-[0_0_12px_var(--glow)]'
              : 'border-border bg-muted/40 hover:bg-muted'"
            @click="$emit('update:form', { ...form, channel_type: 'forum' })"
          >
            <div class="flex items-center gap-2 text-[13px] font-medium">
              <MessagesSquare class="w-4 h-4 text-primary/80" />
              Forum
            </div>
            <p class="mt-1 text-[11px] text-muted-foreground">
              Title-first topics with threaded replies.
            </p>
          </button>
        </div>
      </div>
    </div>

    <div class="border-t border-border px-6 py-3 flex justify-end gap-2">
      <button
        class="text-[13px] font-medium py-1.5 px-3 rounded-xl border border-border hover:bg-muted transition-all duration-200"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="text-[13px] font-medium py-1.5 px-3 rounded-xl bg-primary text-primary-foreground hover:shadow-[0_0_12px_var(--glow)] transition-all duration-200 disabled:opacity-40"
        :disabled="isSubmitting || !form.name.trim()"
        @click="emit('submit')"
      >
        {{ isSubmitting ? "Creating..." : "Create" }}
      </button>
    </div>
  </AppDialog>
</template>

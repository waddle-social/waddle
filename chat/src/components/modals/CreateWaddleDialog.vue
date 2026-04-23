<script setup lang="ts">
import { Lock, Globe, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { CommunityFormData } from "@/lib/chat-ui";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  form: CommunityFormData;
  isSubmitting: boolean;
}>();

const emit = defineEmits<{
  submit: [];
  "update:form": [form: CommunityFormData];
}>();

function updateField(key: keyof CommunityFormData, value: string | boolean) {
  emit("update:form", { ...arguments[0] } as never);
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="flex h-14 items-center justify-between border-b border-border px-4 sm:px-5">
      <h2 class="text-[16px] font-display font-bold">Create Waddle</h2>
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
          placeholder="My Waddle"
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
          placeholder="What is this waddle about?"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div>
        <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
          Privacy
        </label>
        <div class="flex gap-1.5">
          <button
            class="flex h-9 flex-1 items-center justify-center gap-1.5 rounded-lg border px-3 text-[13px] font-medium transition-all duration-200"
            :class="!form.is_public ? 'border-primary bg-primary/5 text-foreground' : 'border-border hover:bg-muted'"
            @click="$emit('update:form', { ...form, is_public: false })"
          >
            <Lock class="w-3.5 h-3.5" />
            Private
          </button>
          <button
            class="flex h-9 flex-1 items-center justify-center gap-1.5 rounded-lg border px-3 text-[13px] font-medium transition-all duration-200"
            :class="form.is_public ? 'border-primary bg-primary/5 text-foreground' : 'border-border hover:bg-muted'"
            @click="$emit('update:form', { ...form, is_public: true })"
          >
            <Globe class="w-3.5 h-3.5" />
            Public
          </button>
        </div>
      </div>
    </div>

    <div class="flex min-h-14 justify-end gap-2 border-t border-border px-4 py-3 sm:px-5">
      <button
        class="h-9 rounded-lg border border-border px-3 text-[13px] font-medium hover:bg-muted transition-all duration-200"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="h-9 rounded-lg bg-primary px-3 text-[13px] font-medium text-primary-foreground hover:shadow-[0_0_12px_var(--glow)] transition-all duration-200 disabled:opacity-40"
        :disabled="isSubmitting || !form.name.trim()"
        @click="emit('submit')"
      >
        {{ isSubmitting ? "Creating..." : "Create" }}
      </button>
    </div>
  </AppDialog>
</template>

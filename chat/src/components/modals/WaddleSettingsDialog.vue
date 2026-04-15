<script setup lang="ts">
import { Lock, Globe, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { CommunityFormData } from "@/lib/chat-ui";
import type { WaddleSummary } from "@/lib/waddle-api";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  waddle: WaddleSummary | null;
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
    <div class="border-b border-border px-6 py-4 flex items-center justify-between">
      <h2 class="text-[16px] font-display font-bold">Waddle Settings</h2>
      <button class="p-1 rounded-xl hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="px-6 py-4 space-y-4">
      <div>
        <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
          Waddle Name
        </label>
        <input
          :value="form.name"
          class="w-full rounded-xl bg-muted focus:outline-none focus:ring-2 focus:ring-primary/20 px-3 py-2 text-[13px] transition-all duration-200"
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
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div>
        <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
          Privacy
        </label>
        <div class="flex gap-1.5">
          <button
            class="flex-1 text-[13px] font-medium py-2 px-3 rounded-xl transition-all duration-200 flex items-center justify-center gap-1.5 border"
            :class="!form.is_public ? 'border-primary bg-primary/5 text-foreground' : 'border-border hover:bg-muted'"
            @click="$emit('update:form', { ...form, is_public: false })"
          >
            <Lock class="w-3.5 h-3.5" />
            Private
          </button>
          <button
            class="flex-1 text-[13px] font-medium py-2 px-3 rounded-xl transition-all duration-200 flex items-center justify-center gap-1.5 border"
            :class="form.is_public ? 'border-primary bg-primary/5 text-foreground' : 'border-border hover:bg-muted'"
            @click="$emit('update:form', { ...form, is_public: true })"
          >
            <Globe class="w-3.5 h-3.5" />
            Public
          </button>
        </div>
      </div>
    </div>

    <div class="border-t border-border px-6 py-3 flex items-center justify-between">
      <button
        class="text-[13px] font-medium py-1.5 px-3 rounded-xl text-destructive hover:bg-destructive/10 transition-all duration-200"
        @click="emit('delete')"
      >
        Delete Waddle
      </button>
      <div class="flex gap-2">
        <button
          class="text-[13px] font-medium py-1.5 px-3 rounded-xl border border-border hover:bg-muted transition-all duration-200"
          @click="open = false"
        >
          Cancel
        </button>
        <button
          class="text-[13px] font-medium py-1.5 px-3 rounded-xl bg-primary text-primary-foreground hover:shadow-[0_0_12px_var(--glow)] transition-all duration-200 disabled:opacity-40"
          :disabled="isSubmitting || !form.name.trim()"
          @click="emit('save')"
        >
          {{ isSubmitting ? "Saving..." : "Save Changes" }}
        </button>
      </div>
    </div>
  </AppDialog>
</template>

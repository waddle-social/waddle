<script setup lang="ts">
import { ref } from "vue";
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";

const open = defineModel<boolean>("open", { required: true });

const emit = defineEmits<{
  submit: [username: string];
}>();

const username = ref("");

function handleSubmit() {
  const trimmed = username.value.replace(/^@/, "").trim();
  if (!trimmed) return;
  emit("submit", trimmed);
  username.value = "";
  open.value = false;
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="flex h-14 items-center justify-between border-b border-border px-4 sm:px-5">
      <h2 class="text-[16px] font-display font-bold">New Message</h2>
      <button class="flex h-8 w-8 items-center justify-center rounded-lg hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <form class="px-4 py-4 sm:px-5" @submit.prevent="handleSubmit">
      <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
        Username
      </label>
      <input
        v-model="username"
        class="h-9 w-full rounded-lg bg-muted px-3 text-[13px] transition-all duration-200 focus:outline-none focus:ring-2 focus:ring-primary/20"
        placeholder="alice"
        autofocus
      />
    </form>

    <div class="flex min-h-14 justify-end gap-2 border-t border-border px-4 py-3 sm:px-5">
      <button
        class="h-9 rounded-lg border border-border px-3 text-[13px] font-medium hover:bg-muted transition-all duration-200"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="h-9 rounded-lg bg-primary px-3 text-[13px] font-medium text-primary-foreground hover:shadow-[0_0_12px_var(--glow)] transition-all duration-200 disabled:opacity-40"
        :disabled="!username.replace(/^@/, '').trim()"
        @click="handleSubmit"
      >
        Start Conversation
      </button>
    </div>
  </AppDialog>
</template>

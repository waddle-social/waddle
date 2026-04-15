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
    <div class="border-b border-border px-6 py-4 flex items-center justify-between">
      <h2 class="text-[16px] font-display font-bold">New Message</h2>
      <button class="p-1 rounded-xl hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <form class="px-6 py-4" @submit.prevent="handleSubmit">
      <label class="block text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground mb-1.5">
        Username
      </label>
      <input
        v-model="username"
        class="w-full rounded-xl bg-muted focus:outline-none focus:ring-2 focus:ring-primary/20 px-3 py-2 text-[13px] transition-all duration-200"
        placeholder="alice"
        autofocus
      />
    </form>

    <div class="border-t border-border px-6 py-3 flex justify-end gap-2">
      <button
        class="text-[13px] font-medium py-1.5 px-3 rounded-xl border border-border hover:bg-muted transition-all duration-200"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="text-[13px] font-medium py-1.5 px-3 rounded-xl bg-primary text-primary-foreground hover:shadow-[0_0_12px_var(--glow)] transition-all duration-200 disabled:opacity-40"
        :disabled="!username.replace(/^@/, '').trim()"
        @click="handleSubmit"
      >
        Start Conversation
      </button>
    </div>
  </AppDialog>
</template>

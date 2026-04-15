<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";

const props = defineProps<{
  open: boolean;
  username: string;
  avatarUrl?: string | null;
  presenceText?: string;
  canMessage?: boolean;
}>();

const emit = defineEmits<{
  close: [];
  message: [];
}>();

const panelEl = ref<HTMLElement | null>(null);

function onWindowClick(event: MouseEvent) {
  if (!props.open) return;
  const target = event.target as Node | null;
  if (!panelEl.value || !target) return;
  if (!panelEl.value.contains(target)) emit("close");
}

function onEsc(event: KeyboardEvent) {
  if (event.key === "Escape" && props.open) emit("close");
}

onMounted(() => {
  window.addEventListener("mousedown", onWindowClick);
  window.addEventListener("keydown", onEsc);
});

onUnmounted(() => {
  window.removeEventListener("mousedown", onWindowClick);
  window.removeEventListener("keydown", onEsc);
});
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed z-50 top-24 left-1/2 -translate-x-1/2 w-64 rounded-xl border border-border bg-background/95 backdrop-blur shadow-xl p-3"
      ref="panelEl"
    >
      <div class="flex items-center gap-3">
        <AppAvatar :name="username" :src="avatarUrl ?? null" />
        <div class="min-w-0">
          <div class="text-[14px] font-semibold truncate">{{ username }}</div>
          <div class="text-[12px] text-muted-foreground">{{ presenceText ?? "offline" }}</div>
        </div>
      </div>
      <button
        v-if="canMessage"
        class="mt-3 w-full h-9 rounded-lg bg-primary text-primary-foreground text-[13px] font-semibold hover:shadow-[0_0_16px_var(--glow-strong)] transition-all duration-200"
        @click="emit('message')"
      >
        Message
      </button>
    </div>
  </Teleport>
</template>

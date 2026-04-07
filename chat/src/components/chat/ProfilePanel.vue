<script setup lang="ts">
import { LogOut } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { WaddleSession } from "@/lib/server-auth";

defineProps<{
  session: WaddleSession;
}>();

const emit = defineEmits<{
  logout: [];
}>();
</script>

<template>
  <div class="border-t border-foreground p-4 flex items-center gap-3">
    <AppAvatar :name="session.username" size="sm" />
    <div class="flex-1 min-w-0">
      <div class="text-sm font-mono font-bold truncate">{{ session.username }}</div>
      <div class="text-[10px] font-mono text-muted-foreground truncate">{{ session.jid }}</div>
    </div>
    <button
      class="h-7 w-7 flex items-center justify-center hover:bg-destructive hover:text-destructive-foreground transition-colors flex-shrink-0"
      title="Log out"
      @click="emit('logout')"
    >
      <LogOut class="w-3.5 h-3.5" />
    </button>
  </div>
</template>

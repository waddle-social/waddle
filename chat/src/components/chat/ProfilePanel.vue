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
  <div class="h-14 border-t border-foreground px-6 flex items-center gap-3 flex-shrink-0">
    <AppAvatar :name="session.username" size="sm" />
    <span class="flex-1 min-w-0 text-sm font-mono font-bold truncate">{{ session.username }}</span>
    <button
      class="h-7 w-7 flex items-center justify-center hover:bg-destructive hover:text-destructive-foreground transition-colors flex-shrink-0"
      title="Log out"
      @click="emit('logout')"
    >
      <LogOut class="w-3.5 h-3.5" />
    </button>
  </div>
</template>

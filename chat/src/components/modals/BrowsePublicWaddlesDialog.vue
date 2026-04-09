<script setup lang="ts">
import { computed } from "vue";
import { Search, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { WaddleSummary } from "@/lib/waddle-api";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  spaces: WaddleSummary[];
  joinedWaddleIds: string[];
  isLoading: boolean;
  joiningWaddleId: string | null;
  query: string;
}>();

const emit = defineEmits<{
  "update:query": [value: string];
  refresh: [];
  join: [waddleId: string];
}>();

const joinedSet = computed(() => new Set(props.joinedWaddleIds));
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="border-b border-foreground p-6 flex items-center justify-between">
      <h2 class="text-xl font-mono font-bold uppercase tracking-wider">Browse Public Spaces</h2>
      <button class="p-1 hover:bg-muted transition-colors" @click="open = false">
        <X class="w-5 h-5" />
      </button>
    </div>

    <div class="p-6 border-b border-foreground space-y-3">
      <div class="relative">
        <Search class="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
        <input
          :value="query"
          class="w-full font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground pl-10 pr-3 py-2 bg-background text-sm"
          placeholder="Search by name or id..."
          @input="emit('update:query', ($event.target as HTMLInputElement).value)"
          @keydown.enter.prevent="emit('refresh')"
        />
      </div>

      <button
        class="font-mono uppercase tracking-wider text-sm py-2 px-4 border border-foreground hover:bg-muted transition-colors"
        :disabled="isLoading"
        @click="emit('refresh')"
      >
        {{ isLoading ? "Loading..." : "Search" }}
      </button>
    </div>

    <div class="p-6 max-h-96 overflow-auto space-y-2">
      <div
        v-for="space in spaces"
        :key="space.id"
        class="border border-foreground p-4 flex items-start gap-4"
      >
        <div class="flex-1 min-w-0">
          <div class="font-mono font-bold text-sm uppercase tracking-wider truncate">
            {{ space.name }}
          </div>
          <div class="font-mono text-xs text-muted-foreground truncate">
            {{ space.id }}
          </div>
          <p v-if="space.description" class="font-mono text-sm mt-2 text-muted-foreground">
            {{ space.description }}
          </p>
        </div>

        <button
          class="font-mono uppercase tracking-wider text-xs py-2 px-3 border border-foreground hover:bg-foreground hover:text-background transition-colors disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-inherit"
          :disabled="joinedSet.has(space.id) || joiningWaddleId === space.id"
          @click="emit('join', space.id)"
        >
          {{
            joinedSet.has(space.id)
              ? "Joined"
              : joiningWaddleId === space.id
                ? "Joining..."
                : "Join"
          }}
        </button>
      </div>

      <div v-if="!isLoading && spaces.length === 0" class="text-center py-8 text-sm font-mono text-muted-foreground">
        No public spaces found
      </div>
    </div>
  </AppDialog>
</template>

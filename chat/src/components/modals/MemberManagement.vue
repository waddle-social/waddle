<script setup lang="ts">
import { X, Search } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { MemberSummary, UserSearchResult } from "@/lib/waddle-api";
import type { EditableRole } from "@/lib/chat-ui";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  members: MemberSummary[];
  memberQuery: string;
  newMemberRole: EditableRole;
  searchResults: UserSearchResult[];
  isSearching: boolean;
  canManageMembers: boolean;
}>();

const emit = defineEmits<{
  "update:memberQuery": [value: string];
  "update:newMemberRole": [value: EditableRole];
  addMember: [userId: string];
  updateRole: [member: MemberSummary, role: EditableRole];
  removeMember: [member: MemberSummary];
}>();

const roles: EditableRole[] = ["member", "moderator", "admin"];
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="border-b border-foreground p-6 flex items-center justify-between">
      <h2 class="text-xl font-mono font-bold uppercase tracking-wider">Members</h2>
      <button class="p-1 hover:bg-muted transition-colors" @click="open = false">
        <X class="w-5 h-5" />
      </button>
    </div>

    <!-- Search -->
    <div v-if="canManageMembers" class="p-6 border-b border-foreground space-y-3">
      <div class="relative">
        <Search class="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
        <input
          :value="memberQuery"
          class="w-full font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground pl-10 pr-3 py-2 bg-background text-sm"
          placeholder="Search users to add..."
          @input="$emit('update:memberQuery', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <div class="flex gap-2">
        <button
          v-for="role in roles"
          :key="role"
          class="font-mono uppercase tracking-wider text-xs py-1 px-3 border transition-colors"
          :class="newMemberRole === role ? 'bg-foreground text-background border-foreground' : 'border-foreground hover:bg-muted'"
          @click="$emit('update:newMemberRole', role)"
        >
          {{ role }}
        </button>
      </div>

      <!-- Search results -->
      <div v-if="searchResults.length > 0" class="space-y-1">
        <button
          v-for="user in searchResults"
          :key="user.id"
          class="w-full flex items-center gap-3 p-3 border border-foreground hover:bg-muted transition-colors text-left"
          @click="emit('addMember', user.id)"
        >
          <AppAvatar :name="user.display_name || user.username" size="sm" />
          <div class="flex-1 min-w-0">
            <div class="font-mono text-sm font-bold truncate">{{ user.display_name || user.username }}</div>
            <div class="font-mono text-xs text-muted-foreground">@{{ user.username }}</div>
          </div>
          <span class="text-xs font-mono uppercase tracking-wider text-muted-foreground">
            Add as {{ newMemberRole }}
          </span>
        </button>
      </div>

      <div v-if="isSearching" class="text-sm font-mono text-muted-foreground">
        Searching...
      </div>
    </div>

    <!-- Member list -->
    <div class="p-6 max-h-96 overflow-auto space-y-2">
      <div
        v-for="member in members"
        :key="member.user_id"
        class="flex items-center gap-3 p-3 border border-foreground"
      >
        <AppAvatar :name="member.username" size="sm" />
        <div class="flex-1 min-w-0">
          <div class="font-mono text-sm font-bold truncate">{{ member.username }}</div>
          <div class="font-mono text-xs text-muted-foreground uppercase tracking-wider">
            {{ member.role }}
          </div>
        </div>
        <div v-if="canManageMembers && member.role !== 'owner'" class="flex gap-1">
          <select
            :value="member.role"
            class="font-mono text-xs border border-foreground bg-background px-2 py-1"
            @change="emit('updateRole', member, ($event.target as HTMLSelectElement).value as EditableRole)"
          >
            <option v-for="role in roles" :key="role" :value="role">{{ role }}</option>
          </select>
          <button
            class="p-1 border border-destructive text-destructive hover:bg-destructive hover:text-destructive-foreground transition-colors"
            @click="emit('removeMember', member)"
          >
            <X class="w-3 h-3" />
          </button>
        </div>
      </div>

      <div v-if="members.length === 0" class="text-center py-4 text-sm font-mono text-muted-foreground">
        No members
      </div>
    </div>
  </AppDialog>
</template>

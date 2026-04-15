<script setup lang="ts">
import { X, Search } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { MemberSummary, UserSearchResult } from "@/lib/waddle-api";
import type { EditableRole } from "@/lib/chat-ui";
import type { RoomPresence } from "@/lib/xmpp-client";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  members: MemberSummary[];
  memberQuery: string;
  newMemberRole: EditableRole;
  searchResults: UserSearchResult[];
  isSearching: boolean;
  canManageMembers: boolean;
  roomPresence?: RoomPresence;
  roomLastSeen?: Record<string, number>;
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
    <div class="border-b border-border px-6 py-4 flex items-center justify-between">
      <h2 class="text-[16px] font-display font-bold">Members</h2>
      <button class="p-1 rounded-xl hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <!-- Search -->
    <div v-if="canManageMembers" class="px-6 py-3 border-b border-border space-y-3">
      <div class="relative">
        <Search class="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
        <input
          :value="memberQuery"
          class="w-full rounded-xl bg-muted focus:outline-none focus:ring-2 focus:ring-primary/20 pl-9 pr-3 py-2 text-[13px] transition-all duration-200"
          placeholder="Search users to add..."
          @input="$emit('update:memberQuery', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <div class="flex gap-1">
        <button
          v-for="role in roles"
          :key="role"
          class="text-[11px] font-medium py-1 px-2.5 rounded-xl capitalize transition-all duration-200 border"
          :class="newMemberRole === role ? 'border-primary bg-primary/5 text-foreground' : 'border-border hover:bg-muted'"
          @click="$emit('update:newMemberRole', role)"
        >
          {{ role }}
        </button>
      </div>

      <!-- Search results -->
      <div v-if="searchResults.length > 0" class="space-y-px">
        <button
          v-for="user in searchResults"
          :key="user.id"
          class="w-full flex items-center gap-2.5 p-2.5 rounded-xl bg-surface hover:bg-muted transition-all duration-200 text-left border border-border"
          @click="emit('addMember', user.id)"
        >
          <AppAvatar :name="user.display_name || user.username" :src="user.avatar_url" size="sm" />
          <div class="flex-1 min-w-0">
            <div class="text-[13px] font-medium truncate">{{ user.display_name || user.username }}</div>
            <div class="text-[11px] text-muted-foreground">@{{ user.username }}</div>
          </div>
          <span class="text-[11px] text-muted-foreground capitalize">
            Add as {{ newMemberRole }}
          </span>
        </button>
      </div>

      <div v-if="isSearching" class="text-[13px] text-muted-foreground flex items-center gap-1.5">
        <span class="typing-dot" />
        <span class="typing-dot" />
        <span class="typing-dot" />
        <span class="ml-1">Searching...</span>
      </div>
    </div>

    <!-- Member list -->
    <div class="px-6 py-3 max-h-80 overflow-auto space-y-px">
      <div
        v-for="member in members"
        :key="member.user_id"
        class="flex items-center gap-2.5 p-2.5 rounded-xl hover:bg-muted/50 transition-all duration-200"
      >
        <AppAvatar :name="member.username" :src="member.avatar_url" :presence="roomPresence?.[member.username] ?? 'offline'" :last-seen="roomLastSeen?.[member.username]" size="sm" />
        <div class="flex-1 min-w-0">
          <div class="text-[13px] font-medium truncate">{{ member.username }}</div>
          <div class="text-[11px] text-muted-foreground capitalize">
            {{ member.role }}
          </div>
        </div>
        <div v-if="canManageMembers && member.role !== 'owner'" class="flex items-center gap-1.5">
          <select
            :value="member.role"
            class="text-[11px] rounded-xl bg-muted px-2 py-1 capitalize focus:outline-none focus:ring-2 focus:ring-primary/20"
            @change="emit('updateRole', member, ($event.target as HTMLSelectElement).value as EditableRole)"
          >
            <option v-for="role in roles" :key="role" :value="role">{{ role }}</option>
          </select>
          <button
            class="p-1 rounded-xl text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-all duration-200"
            @click="emit('removeMember', member)"
          >
            <X class="w-3 h-3" />
          </button>
        </div>
      </div>

      <div v-if="members.length === 0" class="text-center py-6 text-[13px] text-muted-foreground">
        No members
      </div>
    </div>
  </AppDialog>
</template>

<script setup lang="ts">
import { X, Search } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { MemberSummary, UserSearchResult } from "@/lib/chat-types";
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

const roles: EditableRole[] = ["member", "admin", "owner", "outcast"];
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">Members</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close members dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <!-- Search -->
    <div v-if="canManageMembers" class="chat-panel-stack border-b border-border px-4 py-3 sm:px-5">
      <div class="relative">
        <Search class="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
        <input
          :value="memberQuery"
          class="chat-field-control chat-field-control--search type-field"
          placeholder="Search users to add…"
          aria-label="Search users to add"
          @input="$emit('update:memberQuery', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <div class="flex gap-1.5">
        <button
          v-for="role in roles"
          :key="role"
          class="type-control chat-action-button border capitalize"
          :class="newMemberRole === role
            ? 'border-primary/30 bg-primary/8 text-foreground shadow-[0_0_12px_var(--glow)]'
            : 'border-border bg-muted/40 hover:bg-muted'"
          type="button"
          :aria-pressed="newMemberRole === role"
          @click="$emit('update:newMemberRole', role)"
        >
          {{ role }}
        </button>
      </div>

      <!-- Search results -->
      <div v-if="searchResults.length > 0" class="flex flex-col gap-1">
        <button
          v-for="user in searchResults"
          :key="user.id"
          class="chat-list-row flex min-h-12 w-full items-center gap-2.5 border border-border bg-muted/40 p-2 text-left hover:bg-muted"
          type="button"
          @click="emit('addMember', user.id)"
        >
          <AppAvatar :name="user.display_name || user.username" :src="user.avatar_url" size="sm" />
          <div class="flex-1 min-w-0">
            <div class="type-control truncate">{{ user.display_name || user.username }}</div>
            <div class="type-caption text-muted-foreground">@{{ user.username }}</div>
          </div>
          <span class="type-caption text-muted-foreground capitalize">
            Add as {{ newMemberRole }}
          </span>
        </button>
      </div>

      <div v-if="isSearching" class="type-caption text-muted-foreground flex items-center gap-1.5">
        <span class="typing-dot" />
        <span class="typing-dot" />
        <span class="typing-dot" />
        <span>Searching…</span>
      </div>
    </div>

    <!-- Member list -->
    <div class="flex max-h-96 flex-col gap-1 overflow-auto px-4 py-3 sm:px-5">
      <div
        v-for="member in members"
        :key="member.jid"
        class="chat-list-row flex min-h-12 items-center gap-2.5 p-2 hover:bg-muted/50"
      >
        <AppAvatar :name="member.username" :src="member.avatar_url" :presence="roomPresence?.[member.username] ?? 'offline'" :last-seen="roomLastSeen?.[member.username]" size="sm" />
        <div class="flex-1 min-w-0">
          <div class="type-control truncate">{{ member.username }}</div>
          <div class="type-caption text-muted-foreground capitalize">
            {{ member.role }}
          </div>
        </div>
        <div v-if="canManageMembers && member.role !== 'owner'" class="flex items-center gap-1.5">
          <select
            :value="member.role"
            class="type-caption type-emphasis h-8 appearance-none rounded-lg border border-border bg-muted/60 px-2.5 capitalize transition-all duration-200 focus:outline-none focus:ring-2 focus:ring-primary/20"
            :aria-label="`Role for ${member.username}`"
            @change="emit('updateRole', member, ($event.target as HTMLSelectElement).value as EditableRole)"
          >
            <option v-for="role in roles" :key="role" :value="role">{{ role }}</option>
          </select>
          <button
            class="chat-icon-button text-muted-foreground hover:text-destructive hover:bg-destructive/10"
            type="button"
            :aria-label="`Remove ${member.username}`"
            @click="emit('removeMember', member)"
          >
            <X class="w-3.5 h-3.5" />
          </button>
        </div>
      </div>

      <div v-if="members.length === 0" class="type-caption text-center py-6 text-muted-foreground">
        No members
      </div>
    </div>
  </AppDialog>
</template>

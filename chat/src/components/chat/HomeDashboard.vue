<script setup lang="ts">
import { computed } from "vue";
import { Hash, MessageCircle, MessagesSquare, Users } from "lucide-vue-next";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { RosterContact } from "@/lib/xmpp/types";
import { isForumChannel } from "@/lib/channel-types";
import { groupChannelsBySpace } from "@/lib/channel-grouping";

const props = defineProps<{
  spaces: SpaceSummary[];
  channels: ChannelSummary[];
  contacts: RosterContact[];
  isLoading: boolean;
}>();

const emit = defineEmits<{
  selectChannel: [id: string];
  selectContact: [jid: string];
  openNav: [];
}>();

const groups = computed(() => groupChannelsBySpace(props.spaces, props.channels));
const channelsBySpace = computed(() =>
  new Map(groups.value.filter((group) => group.space).map((group) => [group.id, group.channels.length])),
);

function contactLabel(contact: RosterContact): string {
  return contact.name || contact.username || contact.jid;
}

function selectFirstSpaceChannel(spaceId: string) {
  const channel = groups.value.find((group) => group.id === spaceId)?.channels[0];
  if (channel) emit("selectChannel", channel.id);
}
</script>

<template>
  <div class="chat-pane-scroll flex-1 overflow-auto bg-background px-[var(--chat-content-inline)] py-6">
    <div class="mx-auto grid w-full max-w-6xl gap-6">
      <header class="flex items-center justify-between gap-4">
        <div>
          <h1 class="type-display-title">Home</h1>
          <p class="type-caption text-muted-foreground">Spaces, channels, and roster contacts.</p>
        </div>
        <button
          class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground lg:hidden"
          type="button"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Hash class="h-4 w-4" />
        </button>
      </header>

      <section class="grid gap-3">
        <div class="flex items-center gap-2">
          <MessagesSquare class="h-4 w-4 text-primary" />
          <h2 class="type-pane-title">Spaces</h2>
        </div>
        <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
          <button
            v-for="space in spaces"
            :key="space.id"
            class="chat-list-row flex min-h-16 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3 text-left hover:bg-muted/60"
            type="button"
            @click="selectFirstSpaceChannel(space.id)"
          >
            <span class="flex h-9 w-9 items-center justify-center rounded-lg bg-primary/10 text-primary">
              {{ (space.name[0] ?? "S").toUpperCase() }}
            </span>
            <span class="min-w-0 flex-1">
              <span class="type-control block truncate text-foreground">{{ space.name }}</span>
              <span class="type-caption text-muted-foreground">{{ channelsBySpace.get(space.id) ?? 0 }} channels</span>
            </span>
          </button>
          <div v-if="!isLoading && spaces.length === 0" class="type-caption rounded-lg border border-border px-4 py-6 text-muted-foreground">
            No spaces discovered.
          </div>
        </div>
      </section>

      <section class="grid gap-3">
        <div class="flex items-center gap-2">
          <Hash class="h-4 w-4 text-primary" />
          <h2 class="type-pane-title">Channels</h2>
        </div>
        <div class="grid gap-4 lg:grid-cols-2">
          <div
            v-for="group in groups"
            :key="group.id"
            class="rounded-lg border border-border bg-card p-3"
          >
            <h3 class="type-section-label px-1 pb-2 text-muted-foreground">{{ group.name }}</h3>
            <div class="grid gap-1">
              <button
                v-for="channel in group.channels"
                :key="channel.id"
                class="chat-list-row flex min-h-10 items-center gap-2 rounded-md px-3 py-2 text-left text-muted-foreground hover:bg-muted hover:text-foreground"
                type="button"
                @click="emit('selectChannel', channel.id)"
              >
                <component :is="isForumChannel(channel) ? MessagesSquare : Hash" class="h-3.5 w-3.5 text-primary/70" />
                <span class="type-control flex-1 truncate">{{ channel.name }}</span>
              </button>
            </div>
          </div>
          <div v-if="!isLoading && groups.length === 0" class="type-caption rounded-lg border border-border px-4 py-6 text-muted-foreground">
            No channels discovered.
          </div>
        </div>
      </section>

      <section class="grid gap-3">
        <div class="flex items-center gap-2">
          <Users class="h-4 w-4 text-primary" />
          <h2 class="type-pane-title">Members</h2>
        </div>
        <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
          <button
            v-for="contact in contacts"
            :key="contact.jid"
            class="chat-list-row flex min-h-12 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3 text-left hover:bg-muted/60"
            type="button"
            @click="emit('selectContact', contact.jid)"
          >
            <span class="flex h-8 w-8 items-center justify-center rounded-lg bg-muted text-muted-foreground">
              <MessageCircle class="h-3.5 w-3.5" />
            </span>
            <span class="min-w-0 flex-1">
              <span class="type-control block truncate text-foreground">{{ contactLabel(contact) }}</span>
              <span class="type-caption block truncate text-muted-foreground" :title="contact.jid">{{ contact.jid }}</span>
            </span>
          </button>
          <div v-if="!isLoading && contacts.length === 0" class="type-caption rounded-lg border border-border px-4 py-6 text-muted-foreground">
            No roster contacts yet.
          </div>
        </div>
      </section>
    </div>
  </div>
</template>

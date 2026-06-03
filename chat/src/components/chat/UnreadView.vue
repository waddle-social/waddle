<script setup lang="ts">
import { computed, ref } from "vue";
import { Hash, Inbox, ListTree, Menu, RefreshCw } from "lucide-vue-next";
import { connectionStore } from "@/lib/connection-store";
import type { ChannelSummary } from "@/lib/chat-types";
import type { InboxState } from "@/services/inbox";
import { useUnreadOverview } from "@/lib/unread-overview-state";
import UnreadMessageRow from "@/components/chat/UnreadMessageRow.vue";

const props = defineProps<{
  channels: readonly ChannelSummary[];
  inboxState: InboxState;
  onSelectChannel: (channelId: string) => void | Promise<void>;
  onSelectThread: (channelId: string, threadId: string) => void | Promise<void>;
  onRefreshInbox?: () => void | Promise<unknown>;
}>();

const emit = defineEmits<{
  openNav: [];
}>();

const { groups, isLoading, error, refresh } = useUnreadOverview({
  xmppClient: computed(() => connectionStore.client),
  session: computed(() => connectionStore.session),
  channels: computed(() => props.channels),
  inboxState: computed(() => props.inboxState),
});

const hasGroups = computed(() => groups.value.length > 0);
const isRefreshingInbox = ref(false);
const isRefreshBusy = computed(() => isLoading.value || isRefreshingInbox.value);

async function refreshUnread() {
  if (isRefreshBusy.value) return;
  isRefreshingInbox.value = true;
  try {
    await props.onRefreshInbox?.();
    await refresh();
  } finally {
    isRefreshingInbox.value = false;
  }
}
</script>

<template>
  <div class="chat-content-pane">
    <header class="md:hidden flex items-center gap-2 border-b border-border bg-background px-[var(--chat-content-inline)] py-3">
      <button
        type="button"
        class="inline-flex h-8 w-8 items-center justify-center rounded-md border border-transparent text-muted-foreground hover:bg-muted/50 hover:text-foreground"
        aria-label="Open navigation"
        @click="emit('openNav')"
      >
        <Menu class="h-4 w-4" aria-hidden="true" />
      </button>
      <span class="flex h-9 w-9 items-center justify-center rounded-full bg-primary/10 text-primary">
        <Inbox class="h-4.5 w-4.5" aria-hidden="true" />
      </span>
      <h1 class="type-pane-title text-foreground leading-tight">Unread</h1>
    </header>

    <div class="chat-panel-stack p-4">
      <div class="flex items-center justify-between gap-2 border-b border-border/70 pb-3">
        <div>
          <h2 class="type-pane-title">Unread</h2>
          <div class="type-caption text-muted-foreground">
            Everything you haven't read yet, grouped by channel and thread.
          </div>
        </div>
        <button
          type="button"
          class="type-caption inline-flex h-8 items-center gap-1.5 rounded-md border border-border px-2.5 text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:opacity-60"
          :disabled="isRefreshBusy"
          aria-label="Refresh unread"
          @click="refreshUnread()"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="isRefreshBusy ? 'animate-spin' : ''" aria-hidden="true" />
          Refresh
        </button>
      </div>

      <div v-if="isLoading && !hasGroups" class="type-caption text-muted-foreground" aria-busy="true">
        Loading unread…
      </div>

      <div v-else-if="error && !hasGroups" class="type-caption text-destructive">
        Couldn't load unread: {{ error }}
      </div>

      <div v-else-if="!hasGroups" class="type-caption text-muted-foreground">
        You're all caught up — nothing unread.
      </div>

      <template v-else>
        <section
          v-for="group in groups"
          :key="group.roomJid"
          class="chat-panel-stack rounded-lg border border-border/70"
        >
          <button
            type="button"
            class="flex w-full items-center gap-2 rounded-t-lg bg-muted/30 px-3 py-2 text-left hover:bg-muted/50"
            @click="props.onSelectChannel(group.channelId)"
          >
            <Hash class="h-4 w-4 flex-shrink-0 text-primary" aria-hidden="true" />
            <span class="type-card-title flex-1 truncate">{{ group.channelName }}</span>
            <span
              v-if="group.channelUnreadCount > 0"
              class="type-count-badge inline-flex min-w-[18px] h-[18px] items-center justify-center rounded-full bg-primary px-1 text-primary-foreground"
              :aria-label="`${group.channelUnreadCount} unread`"
            >{{ group.channelUnreadCount }}</span>
          </button>

          <div v-if="group.channelMessages.length > 0" class="px-1 py-1">
            <UnreadMessageRow
              v-for="message in group.channelMessages"
              :key="message.id"
              :message="message"
            />
          </div>

          <div
            v-for="thread in group.threads"
            :key="thread.threadId"
            class="border-t border-border/60"
          >
            <button
              type="button"
              class="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-muted/40"
              @click="props.onSelectThread(group.channelId, thread.threadId)"
            >
              <ListTree class="h-3.5 w-3.5 flex-shrink-0 text-muted-foreground" aria-hidden="true" />
              <span class="type-section-label flex-1 truncate text-muted-foreground">{{ thread.title }}</span>
              <span
                class="type-count-badge inline-flex min-w-[18px] h-[18px] items-center justify-center rounded-full bg-primary px-1 text-primary-foreground"
                :aria-label="`${thread.unreadCount} unread`"
              >{{ thread.unreadCount }}</span>
            </button>
            <div class="px-1 pb-1">
              <UnreadMessageRow
                v-for="message in thread.messages"
                :key="message.id"
                :message="message"
              />
            </div>
          </div>
        </section>
      </template>
    </div>
  </div>
</template>

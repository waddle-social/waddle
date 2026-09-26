<script setup lang="ts">
import { computed, ref } from "vue";
import { Hash, ListTree, Menu, RefreshCw } from "lucide-vue-next";
import { button, count } from "styled-system/recipes";
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

const countClass = count();
const refreshButtonClass = button({ variant: "quiet", size: "sm" });

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
  <div class="chat-content-pane chat-pane-scroll bg-background">
    <div class="mx-auto grid w-full max-w-3xl gap-5 px-[var(--chat-content-inline)] py-6">
      <header class="flex flex-wrap items-center gap-3">
        <button
          type="button"
          class="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-border bg-card text-muted-foreground hover:bg-muted hover:text-foreground md:hidden"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Menu class="h-4 w-4" aria-hidden="true" />
        </button>
        <div class="min-w-0 flex-1">
          <h1 class="font-display text-[30px] font-bold leading-none tracking-[-0.03em] text-foreground">Unread</h1>
          <p class="type-caption mt-1.5 text-muted-foreground">
            Everything you haven't read yet, grouped by room and thread.
          </p>
        </div>
        <button
          type="button"
          :class="refreshButtonClass"
          :disabled="isRefreshBusy"
          aria-label="Refresh unread"
          @click="refreshUnread()"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="isRefreshBusy ? 'animate-spin' : ''" aria-hidden="true" />
          Refresh
        </button>
      </header>

      <div v-if="isLoading && !hasGroups" class="type-caption text-muted-foreground" aria-busy="true">
        Loading unread…
      </div>

      <div v-else-if="error && !hasGroups" class="type-caption text-destructive-text">
        Couldn't load unread: {{ error }}
      </div>

      <div v-else-if="!hasGroups" class="rounded-2xl border border-dashed border-border px-4 py-8 text-center">
        <p class="font-display text-lg font-semibold text-foreground">All caught up.</p>
        <p class="type-caption mt-1 text-muted-foreground">Nothing unread. The room is yours.</p>
      </div>

      <template v-else>
        <section
          v-for="group in groups"
          :key="group.roomJid"
          class="overflow-hidden rounded-2xl border border-border bg-card"
          :aria-label="group.channelName"
        >
          <button
            type="button"
            class="flex w-full items-center gap-2 border-b border-border px-4 py-3 text-left transition-colors hover:bg-muted"
            @click="props.onSelectChannel(group.channelId)"
          >
            <Hash class="h-4 w-4 flex-shrink-0 text-primary" aria-hidden="true" />
            <span class="min-w-0 flex-1 truncate font-display text-[15px] font-semibold tracking-[-0.01em] text-foreground">{{ group.channelName }}</span>
            <span
              v-if="group.channelUnreadCount > 0"
              :class="countClass"
              :aria-label="`${group.channelUnreadCount} unread`"
            >{{ group.channelUnreadCount }}</span>
          </button>

          <div v-if="group.channelMessages.length > 0" class="px-2 py-1">
            <UnreadMessageRow
              v-for="message in group.channelMessages"
              :key="message.id"
              :message="message"
            />
          </div>

          <div
            v-for="thread in group.threads"
            :key="thread.threadId"
            class="border-t border-border"
          >
            <button
              type="button"
              class="flex w-full items-center gap-2 px-4 py-2 text-left transition-colors hover:bg-muted"
              @click="props.onSelectThread(group.channelId, thread.threadId)"
            >
              <ListTree class="h-3.5 w-3.5 flex-shrink-0 text-muted-foreground" aria-hidden="true" />
              <span class="min-w-0 flex-1 truncate text-[13px] font-medium text-foreground">{{ thread.title }}</span>
              <span
                :class="countClass"
                :aria-label="`${thread.unreadCount} unread`"
              >{{ thread.unreadCount }}</span>
            </button>
            <div class="px-2 pb-1">
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

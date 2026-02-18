<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import { useRouter } from 'vue-router';

import { type RosterItem, type UnlistenFn, useWaddle } from '../composables/useWaddle';

const { addContact, getRoster, listen } = useWaddle();
const router = useRouter();

const entries = ref<RosterItem[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);
const newContactJid = ref('');
const adding = ref(false);

const grouped = computed(() => {
  return entries.value.reduce<Record<string, RosterItem[]>>((acc, entry) => {
    const groups = entry.groups.length > 0 ? entry.groups : ['Ungrouped'];
    for (const group of groups) {
      if (!acc[group]) acc[group] = [];
      acc[group].push(entry);
    }
    return acc;
  }, {});
});

const orderedGroupNames = computed(() =>
  Object.keys(grouped.value).sort((left, right) => left.localeCompare(right))
);

function contactName(entry: RosterItem): string {
  return entry.name?.trim() || entry.jid;
}

function getInitials(name: string): string {
  const parts = name.split(/[@.\s]+/).filter(Boolean);
  if (parts.length >= 2) {
    const first = parts[0]?.[0] ?? '';
    const second = parts[1]?.[0] ?? '';
    return `${first}${second}`.toUpperCase();
  }
  return name.slice(0, 2).toUpperCase();
}

function getAvatarColor(jid: string): string {
  const colors = ['#5865f2', '#57f287', '#fee75c', '#eb459e', '#ed4245', '#3ba55c', '#faa61a', '#e67e22'];
  let hash = 0;
  for (const ch of jid) hash = ch.charCodeAt(0) + ((hash << 5) - hash);
  return colors[Math.abs(hash) % colors.length] ?? colors[0] ?? '#5865f2';
}

function groupEntries(groupName: string): RosterItem[] {
  return grouped.value[groupName] ?? [];
}

async function refreshRoster(): Promise<void> {
  loading.value = true;
  error.value = null;
  try {
    entries.value = await getRoster();
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    loading.value = false;
  }
}

async function submitAddContact(): Promise<void> {
  const jid = newContactJid.value.trim();
  if (!jid || adding.value) return;
  adding.value = true;
  error.value = null;
  try {
    await addContact(jid);
    newContactJid.value = '';
    await refreshRoster();
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    adding.value = false;
  }
}

function openChat(jid: string): void {
  void router.push(`/chat/${encodeURIComponent(jid)}`);
}

const rosterEvents = [
  'xmpp.roster.received',
  'xmpp.roster.updated',
  'xmpp.roster.removed',
  'xmpp.subscription.request',
  'xmpp.subscription.approved',
  'xmpp.presence.changed',
  'system.connection.established',
];

const unlistenFns: UnlistenFn[] = [];

onMounted(async () => {
  await refreshRoster();
  for (const channel of rosterEvents) {
    try {
      const unlisten = await listen(channel, () => { void refreshRoster(); });
      unlistenFns.push(unlisten);
    } catch {
      // Channel not supported by transport — tolerate gracefully
    }
  }
});

onUnmounted(() => {
  while (unlistenFns.length > 0) {
    const unlisten = unlistenFns.pop();
    unlisten?.();
  }
});
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Header bar -->
    <header class="flex h-12 flex-shrink-0 items-center justify-between border-b border-border px-4 shadow-sm">
      <div class="flex items-center gap-2">
        <svg class="h-5 w-5 text-muted" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M17 20h5v-2a3 3 0 00-5.356-1.857M17 20H7m10 0v-2c0-.656-.126-1.283-.356-1.857M7 20H2v-2a3 3 0 015.356-1.857M7 20v-2c0-.656.126-1.283.356-1.857m0 0a5.002 5.002 0 019.288 0M15 7a3 3 0 11-6 0 3 3 0 016 0z" />
        </svg>
        <h2 class="text-base font-semibold text-foreground">Contacts</h2>
        <span class="text-xs text-muted">{{ entries.length }} total</span>
      </div>
    </header>

    <!-- Content -->
    <div class="flex-1 overflow-y-auto p-4">
      <!-- Add contact form -->
      <form class="mb-4 flex gap-2" @submit.prevent="submitAddContact">
        <input
          v-model="newContactJid"
          type="text"
          placeholder="Add a contact — user@example.com"
          class="min-w-0 flex-1 rounded-lg bg-surface-raised px-3 py-2 text-sm text-foreground placeholder-muted outline-none focus:ring-1 focus:ring-accent"
          autocomplete="off"
        />
        <button
          type="submit"
          class="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-accent/80 disabled:opacity-50"
          :disabled="adding || !newContactJid.trim()"
        >
          {{ adding ? 'Adding…' : 'Add' }}
        </button>
      </form>

      <!-- Error -->
      <div v-if="error" class="mb-4 rounded bg-danger/20 px-3 py-2 text-sm text-danger">
        {{ error }}
      </div>

      <p v-if="loading" class="text-sm text-muted">Loading contacts…</p>
      <p v-else-if="entries.length === 0" class="text-sm text-muted">No contacts found. Add someone above to get started.</p>

      <!-- Grouped roster -->
      <div v-else class="space-y-6">
        <section v-for="groupName in orderedGroupNames" :key="groupName">
          <h3 class="mb-2 text-xs font-semibold uppercase tracking-wide text-muted">
            {{ groupName }} — {{ groupEntries(groupName).length }}
          </h3>
          <ul class="space-y-0.5">
            <li
              v-for="entry in groupEntries(groupName)"
              :key="entry.jid"
              class="group flex cursor-pointer items-center gap-3 rounded px-2 py-2 transition-colors hover:bg-hover"
              @click="openChat(entry.jid)"
            >
              <!-- Avatar -->
              <div
                class="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full text-xs font-semibold text-white"
                :style="{ backgroundColor: getAvatarColor(entry.jid) }"
              >
                {{ getInitials(contactName(entry)) }}
              </div>
              <!-- Info -->
              <div class="min-w-0 flex-1">
                <p class="truncate text-sm font-medium text-foreground">{{ contactName(entry) }}</p>
                <p class="truncate text-xs text-muted">{{ entry.jid }}</p>
              </div>
              <!-- Subscription badge -->
              <span class="text-[10px] capitalize text-muted opacity-0 group-hover:opacity-100">
                {{ entry.subscription }}
              </span>
            </li>
          </ul>
        </section>
      </div>
    </div>
  </div>
</template>

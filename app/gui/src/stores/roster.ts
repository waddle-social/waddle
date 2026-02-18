import { computed, ref } from 'vue';
import { defineStore } from 'pinia';

export interface RosterEntry {
  jid: string;
  name: string;
  group: string;
  subscription: 'none' | 'to' | 'from' | 'both';
  presence: 'available' | 'away' | 'dnd' | 'xa' | 'unavailable';
}

export const useRosterStore = defineStore('roster', () => {
  const entries = ref<RosterEntry[]>([
    {
      jid: 'alice@example.com',
      name: 'Alice',
      group: 'Team',
      subscription: 'both',
      presence: 'available',
    },
    {
      jid: 'bob@example.com',
      name: 'Bob',
      group: 'Team',
      subscription: 'from',
      presence: 'away',
    },
    {
      jid: 'ops@example.com',
      name: 'Ops Alerts',
      group: 'Systems',
      subscription: 'to',
      presence: 'unavailable',
    },
  ]);

  const grouped = computed(() => {
    return entries.value.reduce<Record<string, RosterEntry[]>>((acc, entry) => {
      const key = entry.group;
      if (!acc[key]) {
        acc[key] = [];
      }
      acc[key].push(entry);
      return acc;
    }, {});
  });

  return {
    entries,
    grouped,
  };
});

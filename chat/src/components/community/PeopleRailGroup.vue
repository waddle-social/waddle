<script setup lang="ts">
import { ref } from "vue";
import { ChevronDown } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { PeopleRailPerson } from "@/shell/controllers/use-people-rail";

const props = defineProps<{
  title: string;
  count: number;
  people: PeopleRailPerson[];
  /** Ember kicker with a glowing dot: this group is happening right now. */
  live?: boolean;
  /** Render as a collapsed "link" that expands on demand. */
  collapsible?: boolean;
  emptyText?: string;
}>();

const emit = defineEmits<{
  select: [jid: string];
}>();

const expanded = ref(!props.collapsible);

function personLabel(person: PeopleRailPerson): string {
  const parts = [`Message ${person.name}`];
  if (person.statusText) parts.push(person.statusText);
  if (person.inCall && person.status !== "in-huddle" && person.status !== "speaking") parts.push("in a call");
  return parts.join(", ");
}

function ringClass(person: PeopleRailPerson): string {
  if (person.status === "speaking") return "huddle-ring huddle-ring--speaking";
  if (person.status === "in-huddle") return "huddle-ring";
  return "";
}
</script>

<template>
  <section class="people-rail__group" :aria-label="`${title}, ${count}`">
    <div class="people-rail__kicker">
      <button
        v-if="collapsible"
        type="button"
        class="people-rail__more"
        :aria-expanded="expanded"
        @click="expanded = !expanded"
      >
        <span class="community-kicker">{{ title }} · {{ count }}</span>
        <ChevronDown
          class="h-3.5 w-3.5 flex-shrink-0 transition-transform duration-200"
          :class="expanded ? 'rotate-180' : ''"
          aria-hidden="true"
        />
      </button>
      <span v-else class="community-kicker" :class="live ? 'community-kicker--live' : ''">
        <span v-if="live" class="community-ember" aria-hidden="true" />
        {{ title }} · {{ count }}
      </span>
    </div>
    <ul v-if="expanded" class="people-rail__list" role="list">
      <li v-for="person in people" :key="person.jid">
        <button
          type="button"
          class="people-rail__person"
          :aria-label="personLabel(person)"
          @click="emit('select', person.jid)"
        >
          <span :class="ringClass(person)">
            <AppAvatar
              :name="person.name"
              :src="person.avatarUrl"
              :presence="person.presence"
              :in-call="person.inCall && person.status !== 'in-huddle' && person.status !== 'speaking'"
              size="sm"
            />
          </span>
          <span class="people-rail__text">
            <span class="people-rail__name">{{ person.name }}</span>
            <span
              v-if="person.statusText"
              class="people-rail__status"
              :class="person.status === 'speaking' || person.status === 'in-huddle' ? 'people-rail__status--live' : ''"
            >
              {{ person.statusText }}
            </span>
          </span>
          <span v-if="person.status === 'speaking'" class="speaking-bars" aria-hidden="true">
            <span /><span /><span />
          </span>
        </button>
      </li>
      <li v-if="people.length === 0 && emptyText" class="people-rail__empty">
        {{ emptyText }}
      </li>
    </ul>
  </section>
</template>

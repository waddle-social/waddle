<script setup lang="ts">
import { ref } from "vue";
import { ChevronDown } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { kicker, room } from "styled-system/recipes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import type { PeopleRailPerson } from "@/shell/controllers/use-people-rail";

const props = defineProps<{
  title: string;
  count: number;
  people: PeopleRailPerson[];
  /** Ember kicker with a glowing dot: this group is happening right now. */
  live?: boolean;
  /** Render as a collapsed "link" that expands on demand. */
  collapsible?: boolean;
  /** Bots take no DM: rows are plain, with no Message action. */
  bots?: boolean;
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

const kickerRowClass = css({ display: "flex", alignItems: "center", gap: "2", padding: "8px 10px 4px" });
const kickerClass = cx(kicker(), css({ display: "inline-flex", alignItems: "center", gap: "1.5" }));
const liveKickerClass = cx(kickerClass, css({ color: "live.text" }));
const moreClass = css({
  display: "flex",
  alignItems: "center",
  gap: "1",
  width: "100%",
  border: 0,
  bg: "transparent",
  padding: 0,
  color: "fg.muted",
  cursor: "pointer",
  _hover: { color: "fg" },
  "& svg": { width: "14px", height: "14px", flex: "none", transition: "rotate {durations.base} {easings.out}" },
  "&[aria-expanded=true] svg": { rotate: "180deg" },
});
const listClass = css({ display: "flex", flexDirection: "column", gap: "0.5", margin: 0, padding: 0, listStyle: "none" });
const personClass = cx(room(), css({ width: "100%", border: 0, bg: "transparent", textAlign: "left", cursor: "pointer", color: "fg", "div&": { cursor: "default" } }));
const textClass = css({ display: "flex", flexDirection: "column", flex: 1, minWidth: 0, lineHeight: "1.25" });
const nameClass = css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontWeight: 600 });
const statusClass = css({ textStyle: "caption", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const liveStatusClass = cx(statusClass, css({ color: "live.text" }));
const emptyClass = css({ padding: "2px 10px 6px", fontSize: "sm", lineHeight: "1.45", color: "fg.muted" });
const barsClass = css({
  display: "inline-flex",
  alignItems: "flex-end",
  gap: "2px",
  height: "14px",
  flex: "none",
  "& > span": { width: "3px", height: "100%", borderRadius: "2px", bg: "live", transformOrigin: "bottom", animation: "wd-grow 0.9s {easings.inOut} infinite alternate", _motionReduce: { animation: "none" } },
  "& > span:nth-child(2)": { animationDelay: "0.15s" },
  "& > span:nth-child(3)": { animationDelay: "0.3s" },
});
</script>

<template>
  <section :aria-label="`${title}, ${count}`">
    <div :class="kickerRowClass">
      <button
        v-if="collapsible"
        type="button"
        :class="moreClass"
        :aria-expanded="expanded"
        @click="expanded = !expanded"
      >
        <span :class="kickerClass">{{ title }} · {{ count }}</span>
        <ChevronDown aria-hidden="true" />
      </button>
      <span v-else :class="live ? liveKickerClass : kickerClass">
        <span v-if="live" class="wd-live-dot" aria-hidden="true" />
        {{ title }} · {{ count }}
      </span>
    </div>
    <ul v-if="expanded" :class="listClass" role="list">
      <li v-for="person in people" :key="person.jid">
        <component
          :is="bots ? 'div' : 'button'"
          :type="bots ? undefined : 'button'"
          :class="personClass"
          :aria-label="bots ? undefined : personLabel(person)"
          @click="!bots && emit('select', person.jid)"
        >
          <UserAvatar
            :name="person.name"
            :jid="person.jid"
            :presence="person.presence"
            :in-call="person.inCall && person.status !== 'in-huddle' && person.status !== 'speaking'"
            :huddle="person.status === 'in-huddle'"
            :speaking="person.status === 'speaking'"
            size="sm"
          />
          <span :class="textClass">
            <span :class="nameClass">{{ person.name }}</span>
            <span
              v-if="person.statusText"
              :class="person.status === 'speaking' || person.status === 'in-huddle' ? liveStatusClass : statusClass"
            >
              {{ person.statusText }}
            </span>
          </span>
          <span v-if="person.status === 'speaking'" :class="barsClass" aria-hidden="true">
            <span /><span /><span />
          </span>
        </component>
      </li>
      <li v-if="people.length === 0 && emptyText" :class="emptyClass">
        {{ emptyText }}
      </li>
    </ul>
  </section>
</template>

<script setup lang="ts">
import { ref } from "vue";
import EventPlanningPanel from "@/components/community/EventPlanningPanel.vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { newPlanningItem, type PlanningItem } from "@/lib/xmpp/event-planning";
import { css } from "styled-system/css";

// Dev-only, durable browser fixture. It never connects to a Waddle server.
const self = "ada@waddle.test";
const london = "event-london@muc.waddle.test";
const storageKey = "waddle:preview:event-planning";
type Fixture = Record<string, { name: string; items: PlanningItem[] }>;
function initial(): Fixture {
  return { [london]: { name: "Waddle maintainers · London", items: [
    { ...newPlanningItem("event", "Waddle maintainers · London", "event"), author: self, description: "A few days together to work on Waddle, share ideas and explore London.", location: "London, United Kingdom", timezone: "Europe/London", starts_at: "2027-02-12T09:00:00Z", ends_at: "2027-02-14T17:00:00Z", status: "proposed" },
    { ...newPlanningItem("link", "London transport", "transport"), author: self, url: "https://tfl.gov.uk/", category: "Transport", description: "Getting around London and planning journeys.", pinned: true },
    { ...newPlanningItem("itinerary", "Maintainer planning session", "session"), author: self, timezone: "Europe/London", starts_at: "2027-02-12T10:00:00Z", location: "London", status: "confirmed", pinned: true },
    { ...newPlanningItem("task", "Choose a dinner venue", "dinner"), author: self, assignee: "linus@waddle.test", description: "Find somewhere with vegetarian options.", pinned: true },
    { ...newPlanningItem("decision", "Keep Saturday afternoon free", "saturday"), author: self, status: "confirmed", pinned: true },
    { ...newPlanningItem("poll", "When should we have our group dinner?", "dinner-poll"), author: self, options: [{ id: "friday", label: "Friday" }, { id: "saturday", label: "Saturday" }] },
    { ...newPlanningItem("attendance", "Ada", `attendance:${self}`), author: self, attendance: "going" },
    { ...newPlanningItem("travel", "Ada arrives at King's Cross", "arrival"), author: self, starts_at: "2027-02-12T08:30:00Z", timezone: "Europe/London", location: "King's Cross, London" },
    { ...newPlanningItem("expense", "Meeting snacks", "snacks"), author: self, currency: "GBP", amount_minor: 2450, participants: [self, "linus@waddle.test"] },
  ] } };
}
function load(): Fixture { try { return JSON.parse(localStorage.getItem(storageKey) || "null") || initial(); } catch { return initial(); } }
const data = load();
const conversation = ref("");
const persist = () => localStorage.setItem(storageKey, JSON.stringify(data));
const client = {
  ensureJoined: async () => {},
  fetchUserBookmarks: async () => Object.entries(data).map(([jid, room]) => ({ jid, name: room.name, autojoin: true, notifyMode: "on-mention", richPayloadOptIn: false })),
  fetchEventPlanning: async (room: string) => structuredClone(data[room]?.items || []),
  createEventRoom: async (name: string) => { const room = `event-${crypto.randomUUID()}@muc.waddle.test`; data[room] = { name, items: [] }; persist(); return room; },
  publishEventPlanning: async (room: string, item: PlanningItem) => { const canonical = { ...JSON.parse(JSON.stringify(item)), author: item.author || self }; data[room]!.items = [...data[room]!.items.filter((old) => old.id !== item.id), canonical]; if (item.id === "event") data[room]!.name = item.title; persist(); return canonical; },
  retractEventPlanning: async (room: string, id: string) => { data[room]!.items = data[room]!.items.filter((item) => item.id !== id); persist(); },
  listRoomMembers: async () => [{ jid: self, username: "ada", affiliation: "owner", joined_at: "" }, { jid: "linus@waddle.test", username: "linus", affiliation: "member", joined_at: "" }],
  setEventRoomAffiliation: async () => {},
  sendDirectMessage: async () => ({ id: crypto.randomUUID(), state: "sending" }),
  sendGroupMessage: async (_space: string, _room: string, body: string) => { conversation.value = body; return { id: crypto.randomUUID(), state: "sending" }; },
  setRoomNotificationMode: async () => ({ kind: "ok" }),
} as unknown as BrowserXmppClient;
const root = css({ maxWidth: "1100px", marginInline: "auto", padding: "4", color: "fg", bg: "bg", minHeight: "100vh" });
</script>
<template>
  <main :class="root">
    <EventPlanningPanel :client="client" :self-jid="self" @open-conversation="(room) => { if (!conversation) conversation = `Conversation opened: ${room}`; }" />
    <p v-if="conversation" role="status">{{ conversation }}</p>
  </main>
</template>

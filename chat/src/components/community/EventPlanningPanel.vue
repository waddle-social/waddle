<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { CalendarDays, Check, ExternalLink, MessageSquare, Pin, Plus, RefreshCw, Trash2, Users } from "lucide-vue-next";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { MemberSummary } from "@/lib/chat-types";
import { barePeerJid } from "@/lib/xmpp/jid";
import { eventPlanningCalendar, newPlanningItem, planningInstant, planningLocal, safePlanningUrl, type AttendanceStatus, type PlanningItem, type PlanningKind } from "@/lib/xmpp/event-planning";
import { css, cx } from "styled-system/css";
import { btn, input, kicker } from "styled-system/recipes";

const props = defineProps<{ client: BrowserXmppClient | null; selfJid: string | null; requestedRoom?: { jid: string; version: number } | null }>();
const emit = defineEmits<{ openConversation: [roomJid: string] }>();
const rooms = ref<{ jid: string; name: string; notifyMode?: "always" | "on-mention" | "never" | null }[]>([]);
const selectedRoom = ref<string | null>(null);
const roomInput = ref("");
const items = ref<PlanningItem[]>([]);
const members = ref<MemberSummary[]>([]);
const error = ref<string | null>(null);
const busy = ref(false);
const loading = ref(false);
const creating = ref(false);
const createName = ref("");
const editor = ref<PlanningItem | null>(null);
const startsLocal = ref("");
const endsLocal = ref("");
const optionsText = ref("");
const participantsText = ref("");
const amountText = ref("");
const activeTab = ref("overview");
const inviteJid = ref("");
const inviteRole = ref<"member" | "admin">("member");
const notifyMode = ref<"always" | "on-mention" | "never">("on-mention");
let request = 0;
let refreshTimer: ReturnType<typeof setInterval> | undefined;
let destroyed = false;
const self = computed(() => props.selfJid ? barePeerJid(props.selfJid) : "");
const metadata = computed(() => items.value.find((item) => item.id === "event" && item.kind === "event"));
const ownMember = computed(() => members.value.find((member) => member.jid === self.value));
const canOrganise = computed(() => ["owner", "admin"].includes(ownMember.value?.affiliation ?? "") || metadata.value?.author === self.value);
const tabs = [
  { id: "overview", label: "Overview" }, { id: "itinerary", label: "Itinerary" },
  { id: "link", label: "Saved links" }, { id: "task", label: "Tasks" },
  { id: "decision", label: "Decisions" }, { id: "poll", label: "Polls" },
  { id: "people", label: "People" }, { id: "travel", label: "Travel" }, { id: "expense", label: "Expenses" }, { id: "media", label: "Files & photos" },
];
const kindLabels: Record<string, string> = { event: "event details", link: "saved link", task: "task", itinerary: "itinerary entry", decision: "decision", travel: "travel details", expense: "expense", poll: "poll", media: "file or photo" };
const visibleItems = computed(() => {
  const list = activeTab.value === "overview" ? items.value.filter((item) => item.pinned && item.kind !== "event") : activeTab.value === "media" ? items.value.filter((item) => item.kind === "link" && ["photo", "file"].includes(item.category || "")) : items.value.filter((item) => item.kind === activeTab.value);
  return [...list].sort((a, b) => Number(b.pinned) - Number(a.pinned) || (a.starts_at ?? "9999").localeCompare(b.starts_at ?? "9999") || a.title.localeCompare(b.title));
});
const attendance = computed(() => items.value.filter((item) => item.kind === "attendance"));
const myAttendance = computed(() => attendance.value.find((item) => item.author === self.value)?.attendance);
const totals = computed(() => {
  const grouped = new Map<string, number>();
  for (const item of items.value) if (item.kind === "expense" && item.currency && item.amount_minor != null) grouped.set(item.currency, (grouped.get(item.currency) ?? 0) + item.amount_minor);
  return [...grouped].map(([currency, amount]) => money(amount, currency));
});
const primaryClass = btn({ variant: "primary", size: "sm" });
const buttonClass = btn({ size: "sm" });
const inputClass = input();
const ui = {
  root: css({ display: "flex", flexDirection: "column", gap: "4", padding: "5", border: "1px solid {colors.border.soft}", borderRadius: "16px", bg: "surface", minWidth: 0 }),
  row: css({ display: "flex", alignItems: "center", flexWrap: "wrap", gap: "2.5" }),
  between: css({ display: "flex", alignItems: "center", justifyContent: "space-between", flexWrap: "wrap", gap: "3" }),
  title: css({ font: "700 24px/1.25 {fonts.display}", margin: 0 }),
  subtitle: css({ color: "fg.muted", fontSize: "sm", whiteSpace: "pre-wrap" }),
  grid: css({ display: "grid", gap: "3", gridTemplateColumns: "1fr", md: { gridTemplateColumns: "repeat(2,minmax(0,1fr))" } }),
  field: css({ display: "flex", flexDirection: "column", gap: "1.5", fontSize: "sm", minWidth: 0 }),
  card: css({ display: "flex", flexDirection: "column", gap: "2.5", padding: "4", border: "1px solid {colors.border.soft}", borderRadius: "12px", minWidth: 0, overflowWrap: "anywhere" }),
  form: css({ display: "flex", flexDirection: "column", gap: "3", padding: "4", bg: "bg", border: "1px solid {colors.border.soft}", borderRadius: "12px" }),
  tabs: css({ display: "flex", gap: "2", overflowX: "auto", paddingBlock: "1" }),
  selected: css({ borderColor: "accent", bg: "bg.rail" }),
  error: css({ color: "danger", fontSize: "sm" }),
  list: css({ display: "flex", flexDirection: "column", gap: "3" }),
  photo: css({ width: "100%", maxHeight: "240px", objectFit: "contain", borderRadius: "8px" }),
  full: css({ width: "100%", minHeight: "80px", resize: "vertical" }),
};

async function discover() {
  const client = props.client;
  if (!client) { rooms.value = []; return; }
  try {
    const bookmarks = await client.fetchUserBookmarks();
    if (client !== props.client || destroyed) return;
    rooms.value = bookmarks.filter((bookmark) => bookmark.jid.split("@")[0]?.startsWith("event-")).map((bookmark) => ({ jid: bookmark.jid, name: bookmark.name || "Event planning", notifyMode: bookmark.notifyMode }));
  } catch (cause) { if (client === props.client && !destroyed) error.value = message(cause); }
}
function message(cause: unknown): string { return cause instanceof Error ? cause.message : String(cause); }
async function refresh(silent = false) {
  const client = props.client; const room = selectedRoom.value;
  if (!client || !room || busy.value) return;
  const token = ++request;
  if (!silent) { loading.value = true; error.value = null; }
  try {
    const result = await client.fetchEventPlanning(room);
    if (token !== request || room !== selectedRoom.value || client !== props.client || destroyed) return;
    items.value = result;
    const event = result.find((item) => item.id === "event");
    if (event && !rooms.value.some((entry) => entry.jid === room)) rooms.value.push({ jid: room, name: event.title });
    if (event) {
      const saved = rooms.value.find((entry) => entry.jid === room);
      if (saved) saved.name = event.title;
    }
  } catch (cause) { if (token === request && !destroyed) { error.value = message(cause); items.value = []; members.value = []; } }
  finally { if (token === request) loading.value = false; }
}
async function selectRoom(room: string) {
  notifyMode.value = rooms.value.find((entry) => entry.jid === room.trim())?.notifyMode ?? "on-mention";
  request++; selectedRoom.value = room.trim(); items.value = []; members.value = []; editor.value = null; creating.value = false; activeTab.value = "overview";
  const joiningClient = props.client; const joiningRoom = selectedRoom.value;
  try { if (joiningClient && joiningRoom) await joiningClient.ensureJoined(joiningRoom); }
  catch (cause) { if (joiningRoom === selectedRoom.value) error.value = message(cause); return; }
  if (joiningRoom !== selectedRoom.value || joiningClient !== props.client) return;
  await refresh();
  const client = props.client; const selected = selectedRoom.value;
  if (!client || !selected) return;
  try {
    const result = await client.listRoomMembers(selected, { roomJid: selected });
    if (selected === selectedRoom.value && client === props.client && !destroyed) members.value = result.filter((member) => member.affiliation !== "outcast");
  } catch { /* Ordinary members may not be allowed to query affiliations. */ }
}
async function mutate(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true; error.value = null;
  try { await action(); } catch (cause) { error.value = message(cause); }
  finally { busy.value = false; }
}
async function publish(item: PlanningItem) {
  const room = selectedRoom.value; const client = props.client;
  if (!room || !client) throw new Error("Open an event first.");
  const result = await client.publishEventPlanning(room, item);
  if (room === selectedRoom.value && client === props.client) items.value = [result, ...items.value.filter((entry) => entry.id !== result.id)];
}
async function createEvent() {
  const name = createName.value.trim(); const client = props.client;
  if (!name || !client) return;
  await mutate(async () => {
    const room = await client.createEventRoom(name, true);
    if (client !== props.client || destroyed) return;
    selectedRoom.value = room; items.value = []; members.value = [];
    const event = { ...newPlanningItem("event", name, "event"), timezone: Intl.DateTimeFormat().resolvedOptions().timeZone, status: "proposed" as const };
    // Keep this workspace selected if metadata publication fails, so retrying never creates a second room.
    editor.value = event; creating.value = false;
    if (!rooms.value.some((entry) => entry.jid === room)) rooms.value.push({ jid: room, name });
    await publish(event);
    if (client !== props.client || destroyed) return;
    startEdit(event); createName.value = "";
  });
}
function startEdit(item: PlanningItem) {
  editor.value = { ...item, options: item.options.map((option) => ({ ...option })), participants: [...item.participants], selections: [...item.selections], timezone: item.timezone || metadata.value?.timezone || Intl.DateTimeFormat().resolvedOptions().timeZone };
  startsLocal.value = planningLocal(item.starts_at, editor.value.timezone!);
  endsLocal.value = planningLocal(item.ends_at, editor.value.timezone!);
  optionsText.value = item.options.map((option) => option.label).join("\n"); participantsText.value = item.participants.join(", ");
  amountText.value = item.amount_minor == null ? "" : String(item.amount_minor / 10 ** currencyDigits(item.currency || "GBP"));
}
function addItem() {
  if (activeTab.value === "media") startEdit({ ...newPlanningItem("link", ""), category: "file" });
  else if (activeTab.value in kindLabels) startEdit(newPlanningItem(activeTab.value as PlanningKind, ""));
}
function mayDelete(item: PlanningItem) { return canOrganise.value || item.author === self.value; }
function mayEdit(item: PlanningItem) { return item.author === self.value || (item.kind === "task" && item.assignee === self.value) || (canOrganise.value && !["travel", "attendance", "vote"].includes(item.kind)); }
async function saveEditor() {
  const draft = editor.value;
  if (!draft || !draft.title.trim()) return;
  await mutate(async () => {
    const starts_at = planningInstant(startsLocal.value, draft.timezone!);
    const ends_at = planningInstant(endsLocal.value, draft.timezone!);
    if (ends_at && !starts_at) throw new Error("Add a start time before an end time.");
    if (starts_at && ends_at && ends_at <= starts_at) throw new Error("End time must be after start time.");
    const item: PlanningItem = { ...draft, title: draft.title.trim(), url: draft.url?.trim() || null, assignee: draft.assignee?.trim() || null, location: draft.location?.trim() || null, category: draft.category?.trim() || null, starts_at: starts_at ?? null, ends_at: ends_at ?? null, participants: participantsText.value.split(",").map((value) => value.trim()).filter(Boolean) };
    if (item.url && !safePlanningUrl(item.url)) throw new Error("Use a complete http or https link.");
    if (item.kind === "link" && !item.url) throw new Error("Add a link to save.");
    if (item.kind === "poll") {
      const labels = optionsText.value.split("\n").map((label) => label.trim()).filter(Boolean);
      if (labels.length < 2 || new Set(labels).size !== labels.length) throw new Error("Add at least two different poll options, one per line.");
      item.options = labels.map((label) => ({ id: draft.options.find((option) => option.label === label)?.id ?? crypto.randomUUID(), label }));
    }
    if (item.kind === "expense") {
      const amount = Number(amountText.value);
      if (!amountText.value.trim() || !Number.isFinite(amount) || amount < 0) throw new Error("Enter a valid amount.");
      item.currency = item.currency?.toUpperCase() || "GBP";
      item.amount_minor = Math.round(amount * 10 ** currencyDigits(item.currency));
      if (!Number.isSafeInteger(item.amount_minor)) throw new Error("This amount is too large.");
    }
    await publish(item); if (editor.value === draft) editor.value = null;
  });
}
async function remove(item: PlanningItem) {
  if (!window.confirm(`Delete “${item.title}”?`)) return;
  await mutate(async () => {
    const room = selectedRoom.value; if (!room || !props.client) return;
    await props.client.retractEventPlanning(room, item.id);
    if (selectedRoom.value === room) items.value = items.value.filter((entry) => entry.id !== item.id);
  });
}
async function rsvp(status: AttendanceStatus) {
  await mutate(() => publish({ ...newPlanningItem("attendance", self.value, `attendance:${self.value}`), attendance: status }));
}
async function vote(poll: PlanningItem, option: string) {
  await mutate(() => publish({ ...newPlanningItem("vote", poll.title, `vote:${poll.id}:${self.value}`), subject: poll.id, selections: [option] }));
}
function votes(poll: PlanningItem, option: string) { return items.value.filter((item) => item.kind === "vote" && item.subject === poll.id && item.selections.includes(option)).length; }
function myVote(poll: PlanningItem, option: string) { return items.value.some((item) => item.kind === "vote" && item.subject === poll.id && item.author === self.value && item.selections.includes(option)); }
function currencyDigits(currency: string) { return new Intl.NumberFormat("en", { style: "currency", currency }).resolvedOptions().maximumFractionDigits ?? 2; }
function money(amount: number, currency: string) { try { return new Intl.NumberFormat(undefined, { style: "currency", currency }).format(amount / 10 ** currencyDigits(currency)); } catch { return `${amount} ${currency} minor units`; } }
function when(item: PlanningItem) {
  if (!item.starts_at) return "Date to be confirmed";
  const timezone = item.timezone || metadata.value?.timezone || "UTC";
  const format = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short", timeZone: timezone });
  return `${format.format(new Date(item.starts_at))}${item.ends_at ? ` – ${format.format(new Date(item.ends_at))}` : ""} (${timezone})`;
}
function mapUrl(location: string) { return `https://www.openstreetmap.org/search?query=${encodeURIComponent(location)}`; }
function exportCalendar() {
  if (!selectedRoom.value) return;
  const url = URL.createObjectURL(new Blob([eventPlanningCalendar(items.value, selectedRoom.value)], { type: "text/calendar;charset=utf-8" }));
  const anchor = document.createElement("a"); anchor.href = url; anchor.download = "waddle-event.ics"; anchor.click(); URL.revokeObjectURL(url);
}
async function invite() {
  const room = selectedRoom.value; const client = props.client; const jid = inviteJid.value.trim();
  if (!room || !client || !jid) return;
  await mutate(async () => {
    await client.setEventRoomAffiliation(room, jid, inviteRole.value);
    if (client !== props.client || room !== selectedRoom.value || destroyed) return;
    members.value = [...members.value.filter((member) => member.jid !== jid), { jid, username: jid.split("@")[0]!, affiliation: inviteRole.value, joined_at: "" }];
    const invitation = `${metadata.value?.title || "Waddle event"}: you have been invited to plan this event. Open Events in Waddle and enter ${room}. Conversation: xmpp:${room}?join`;
    const sent = await client.sendDirectMessage(jid, invitation);
    inviteJid.value = "";
    if (!sent || sent.state === "rejected") throw new Error("Membership was granted, but the invitation message could not be sent. Share the event address with this person.");
  });
}
async function ensureBookmark(room: string, client: BrowserXmppClient) {
  const bookmarks = await client.fetchUserBookmarks();
  if (bookmarks.some((bookmark) => bookmark.jid === room)) return;
  const saved = await client.setRoomNotificationMode({ roomJid: room, name: metadata.value?.title, mode: "on-mention" });
  if (saved.kind !== "ok") throw new Error("Could not save this event before opening the conversation.");
}
async function openConversation() {
  const room = selectedRoom.value; const client = props.client;
  if (!room || !client) return;
  await mutate(async () => { await ensureBookmark(room, client); if (client === props.client && room === selectedRoom.value && !destroyed) emit("openConversation", room); });
}
async function discuss(item: PlanningItem) {
  const room = selectedRoom.value; const client = props.client;
  if (!room || !client) return;
  await mutate(async () => {
    await ensureBookmark(room, client);
    const body = [item.title, item.description, item.url].filter(Boolean).join("\n");
    const sent = await client.sendGroupMessage("", room, body, { threadId: item.id });
    if (!sent || sent.state === "rejected") throw new Error("Could not share this item to the event conversation.");
    if (client === props.client && room === selectedRoom.value && !destroyed) emit("openConversation", room);
  });
}
async function setNotifications() {
  await mutate(async () => {
    if (!selectedRoom.value || !props.client) return;
    const result = await props.client.setRoomNotificationMode({ roomJid: selectedRoom.value, name: metadata.value?.title, mode: notifyMode.value });
    if (result.kind !== "ok") throw new Error("Could not save event notifications.");
    const saved = rooms.value.find((room) => room.jid === selectedRoom.value);
    if (saved) saved.notifyMode = notifyMode.value;
  });
}
async function rememberRoom() {
  const room = selectedRoom.value; const client = props.client;
  if (!room || !client) return;
  await mutate(async () => { await ensureBookmark(room, client); await discover(); });
}
watch(() => props.requestedRoom, (room) => { if (room) void selectRoom(room.jid); });
watch(() => props.client, () => { request++; items.value = []; members.value = []; selectedRoom.value = null; editor.value = null; void discover(); });
onMounted(() => {
  void discover();
  refreshTimer = setInterval(() => { if (document.visibilityState === "visible" && !editor.value) void refresh(true); }, 20_000);
});
onUnmounted(() => { destroyed = true; request++; if (refreshTimer) clearInterval(refreshTimer); });
</script>

<template>
  <section :class="ui.root" aria-label="Event planning">
    <div :class="ui.between">
      <div><p :class="kicker()">Plan together</p><h2 :class="ui.title">Events &amp; trips</h2></div>
      <div :class="ui.row">
        <button :class="buttonClass" type="button" :disabled="busy || loading" @click="discover(); refresh()"><RefreshCw :size="16" aria-hidden="true" /> Refresh</button>
        <button v-if="client" :class="primaryClass" type="button" :disabled="busy" @click="creating = !creating; editor = null"><Plus :size="16" aria-hidden="true" /> Plan an event</button>
      </div>
    </div>
    <p :class="ui.subtitle">A private space for conversations, useful links, plans and decisions. Only invited members can read the planning space.</p>
    <p v-if="error" role="alert" :class="ui.error">{{ error }}</p>
    <form v-if="creating" :class="ui.form" @submit.prevent="createEvent">
      <label :class="ui.field">Event title<input v-model="createName" :class="inputClass" placeholder="Waddle maintainers in London" required maxlength="200" /></label>
      <p :class="ui.subtitle">Dates and venues can be decided later. This event is private and excluded from the public calendar feed.</p>
      <div :class="ui.row"><button :class="primaryClass" :disabled="busy || !createName.trim()" type="submit">{{ busy ? 'Creating…' : 'Create planning space' }}</button><button :class="buttonClass" type="button" @click="creating = false">Cancel</button></div>
    </form>
    <div v-if="rooms.length" :class="ui.row" aria-label="Your event planning spaces">
      <button v-for="room in rooms" :key="room.jid" :class="cx(buttonClass, selectedRoom === room.jid && ui.selected)" :aria-pressed="selectedRoom === room.jid" :disabled="busy" type="button" @click="selectRoom(room.jid)"><CalendarDays :size="16" aria-hidden="true" />{{ room.name }}</button>
    </div>
    <details v-if="client"><summary>Open an invited event</summary><form :class="ui.row" @submit.prevent="selectRoom(roomInput)"><label :class="ui.field">Event address<input v-model="roomInput" :class="inputClass" placeholder="event-…@muc.example.com" required /></label><button type="submit" :class="buttonClass" :disabled="busy">Open</button></form></details>
    <p v-if="loading" role="status">Loading event…</p>
    <template v-if="selectedRoom && !loading">
      <div :class="ui.between">
        <div><h3 :class="ui.title">{{ metadata?.title || rooms.find(room => room.jid === selectedRoom)?.name || 'Event planning' }}</h3><p :class="ui.subtitle">{{ selectedRoom }}</p></div>
        <div :class="ui.row"><button :class="primaryClass" type="button" @click="openConversation"><MessageSquare :size="16" aria-hidden="true" /> Conversation</button><button :class="buttonClass" type="button" @click="rememberRoom">Save event</button><button :class="buttonClass" type="button" @click="exportCalendar">Export calendar</button></div>
      </div>
      <nav :class="ui.tabs" aria-label="Event sections"><button v-for="tab in tabs" :key="tab.id" :class="cx(buttonClass, activeTab === tab.id && ui.selected)" :aria-pressed="activeTab === tab.id" type="button" @click="activeTab = tab.id; editor = null">{{ tab.label }}</button></nav>
      <div v-if="activeTab === 'overview'" :class="ui.list">
        <template v-if="metadata">
          <p :class="ui.subtitle">{{ metadata.description }}</p>
          <p>{{ when(metadata) }} · {{ metadata.status || 'proposed' }}</p>
          <a v-if="metadata.location" :href="mapUrl(metadata.location)" target="_blank" rel="noopener noreferrer">{{ metadata.location }} <ExternalLink :size="14" aria-hidden="true" /></a>
          <div :class="ui.row"><button v-if="canOrganise" type="button" :class="buttonClass" @click="startEdit(metadata)">Edit event details</button><button v-if="canOrganise" type="button" :class="buttonClass" :disabled="busy" @click="mutate(() => publish({ ...metadata!, status: 'cancelled' }))">Cancel event</button></div>
        </template>
        <button v-if="!metadata && canOrganise" type="button" :class="primaryClass" @click="startEdit(newPlanningItem('event', rooms.find(room => room.jid === selectedRoom)?.name || 'Event planning', 'event'))">Add event details</button>
        <div :class="ui.row" aria-label="Your attendance"><span>Are you coming?</span><button v-for="status in (['interested', 'going', 'declined'] as const)" :key="status" type="button" :class="buttonClass" :disabled="busy" :aria-pressed="myAttendance === status" @click="rsvp(status)">{{ status }}</button></div>
        <p :class="ui.subtitle">{{ attendance.filter(item => item.attendance === 'going').length }} going · {{ attendance.filter(item => item.attendance === 'interested').length }} interested</p>
        <p :class="ui.subtitle">Use the conversation for replies, mentions, files and photos. Pin decisions and plans below so everyone can find them.</p>
        <h4 v-if="visibleItems.length" :class="kicker()">Pinned for everyone</h4>
      </div>
      <div v-if="activeTab === 'people'" :class="ui.list">
        <form v-if="canOrganise" :class="ui.form" @submit.prevent="invite"><h4>Invite a member or co-organiser</h4><div :class="ui.grid"><label :class="ui.field">Waddle address<input v-model="inviteJid" :class="inputClass" type="text" placeholder="name@example.com" required /></label><label :class="ui.field">Role<select v-model="inviteRole" :class="inputClass"><option value="member">Attendee</option><option value="admin">Co-organiser</option></select></label></div><button type="submit" :class="primaryClass" :disabled="busy || !inviteJid.trim()"><Users :size="16" aria-hidden="true" /> Grant access and send invitation</button></form>
        <div v-for="member in members" :key="member.jid" :class="ui.card"><div :class="ui.between"><span>{{ member.jid }} · {{ member.affiliation }}</span><button v-if="canOrganise && member.jid !== self && member.affiliation !== 'owner'" type="button" :class="buttonClass" :disabled="busy" @click="mutate(async () => { await client!.setEventRoomAffiliation(selectedRoom!, member.jid, 'none'); members = members.filter(person => person.jid !== member.jid); })">Remove access</button></div><span :class="ui.subtitle">{{ attendance.find(item => item.author === member.jid)?.attendance || 'Invited · no response yet' }}</span></div>
        <div v-for="person in attendance.filter(person => !members.some(member => member.jid === person.author))" :key="person.id" :class="ui.card">{{ person.author }} · {{ person.attendance }}</div>
        <form :class="ui.row" @submit.prevent="setNotifications"><label :class="ui.field">Event notifications<select v-model="notifyMode" :class="inputClass"><option value="always">All messages</option><option value="on-mention">Mentions only</option><option value="never">Muted</option></select></label><button :class="buttonClass" type="submit" :disabled="busy">Save notifications</button></form>
      </div>
      <div v-if="activeTab !== 'overview' && activeTab !== 'people'" :class="ui.between"><p :class="ui.subtitle">{{ activeTab === 'travel' ? 'Travel details are shared with all invited event members. Add only what you want to share.' : activeTab === 'expense' ? `Recorded expenses${totals.length ? ': ' + totals.join(' · ') : ''}` : 'Keep everyone up to date as plans take shape.' }}</p><button type="button" :class="primaryClass" :disabled="busy" @click="addItem"><Plus :size="16" aria-hidden="true" />Add {{ kindLabels[activeTab] }}</button></div>
      <form v-if="editor" :class="ui.form" @submit.prevent="saveEditor">
        <h4>{{ editor.author ? 'Edit' : 'Add' }} {{ kindLabels[editor.kind] }}</h4>
        <label :class="ui.field">Title<input v-model="editor.title" :class="inputClass" required maxlength="200" /></label>
        <label :class="ui.field">Notes<textarea v-model="editor.description" :class="cx(inputClass, ui.full)" rows="3" /></label>
        <div :class="ui.grid">
          <label v-if="['link', 'itinerary', 'travel', 'decision'].includes(editor.kind)" :class="ui.field">Link<input v-model="editor.url" :class="inputClass" type="url" placeholder="https://…" :required="editor.kind === 'link'" /></label>
          <label v-if="['link', 'task', 'travel'].includes(editor.kind)" :class="ui.field">Category<input v-model="editor.category" :class="inputClass" placeholder="Accommodation, transport, food, photo, file…" /></label>
          <label v-if="['event', 'itinerary', 'travel'].includes(editor.kind)" :class="ui.field">Place<input v-model="editor.location" :class="inputClass" /></label>
          <label v-if="['task', 'itinerary'].includes(editor.kind)" :class="ui.field">Owner's Waddle address<input v-model="editor.assignee" :class="inputClass" placeholder="name@example.com" :disabled="!!editor.author && editor.author !== self && !canOrganise" /></label>
          <template v-if="['event', 'itinerary', 'travel', 'task'].includes(editor.kind)"><label :class="ui.field">{{ editor.kind === 'task' ? 'Due' : 'Starts' }} (optional)<input v-model="startsLocal" :class="inputClass" type="datetime-local" /></label><label v-if="editor.kind !== 'task'" :class="ui.field">Ends (optional)<input v-model="endsLocal" :class="inputClass" type="datetime-local" /></label><label :class="ui.field">Time zone<input v-model="editor.timezone" :class="inputClass" placeholder="Europe/London" required /></label></template>
          <label v-if="['event', 'itinerary', 'decision'].includes(editor.kind)" :class="ui.field">Status<select v-model="editor.status" :class="inputClass"><option value="proposed">Proposed</option><option value="confirmed">Confirmed</option><option value="cancelled">Cancelled</option></select></label>
          <template v-if="editor.kind === 'expense'"><label :class="ui.field">Amount paid<input v-model="amountText" :class="inputClass" type="number" step="any" min="0" required /></label><label :class="ui.field">Currency<input v-model="editor.currency" :class="inputClass" placeholder="GBP" pattern="[A-Za-z]{3}" maxlength="3" /></label><label :class="ui.field">Shared by (Waddle addresses, comma separated)<input v-model="participantsText" :class="inputClass" /></label></template>
        </div>
        <label v-if="editor.kind === 'poll'" :class="ui.field">Options, one per line<textarea v-model="optionsText" :class="cx(inputClass, ui.full)" rows="4" required /></label>
        <label v-if="editor.kind !== 'event'" :class="ui.row"><input v-model="editor.pinned" type="checkbox" /> Pin to event overview</label>
        <div :class="ui.row"><button :class="primaryClass" type="submit" :disabled="busy || !editor.title.trim()">{{ busy ? 'Saving…' : 'Save' }}</button><button :class="buttonClass" type="button" :disabled="busy" @click="editor = null">Cancel</button></div>
      </form>
      <p v-if="!editor && !visibleItems.length && activeTab !== 'overview' && activeTab !== 'people'" :class="ui.subtitle">Nothing here yet. Add the first {{ kindLabels[activeTab] }}.</p>
      <div :class="ui.grid">
        <article v-for="item in visibleItems" :key="item.id" :class="ui.card">
          <div :class="ui.between"><h4><Pin v-if="item.pinned" :size="14" aria-label="Pinned" />{{ item.title }}</h4><span :class="ui.subtitle">{{ item.category || kindLabels[item.kind] }}</span></div>
          <p v-if="item.description" :class="ui.subtitle">{{ item.description }}</p>
          <p v-if="item.starts_at">{{ when(item) }}</p>
          <a v-if="item.location" :href="mapUrl(item.location)" target="_blank" rel="noopener noreferrer">{{ item.location }}</a>
          <img v-if="activeTab === 'media' && item.category === 'photo' && safePlanningUrl(item.url)" :class="ui.photo" :src="safePlanningUrl(item.url)!" :alt="item.title" loading="lazy" referrerpolicy="no-referrer" />
          <a v-if="safePlanningUrl(item.url)" :href="safePlanningUrl(item.url)!" target="_blank" rel="noopener noreferrer">{{ item.url }} <ExternalLink :size="14" aria-hidden="true" /></a>
          <p v-if="item.assignee" :class="ui.subtitle">Owner: {{ item.assignee }}</p>
          <p v-if="item.kind === 'expense' && item.amount_minor != null">{{ money(item.amount_minor, item.currency || 'GBP') }} · Paid by {{ item.author }}<span v-if="item.participants.length"> · Shared by {{ item.participants.join(', ') }}</span></p>
          <p v-if="item.status">{{ item.status }}</p>
          <div v-if="item.kind === 'poll'" :class="ui.list"><button v-for="option in item.options" :key="option.id" type="button" :class="buttonClass" :disabled="busy || item.completed" :aria-pressed="myVote(item, option.id)" @click="vote(item, option.id)"><Check v-if="myVote(item, option.id)" :size="14" aria-hidden="true" />{{ option.label }} · {{ votes(item, option.id) }}</button><span v-if="item.completed">Poll closed</span></div>
          <div v-if="mayEdit(item)" :class="ui.row"><button v-if="['task', 'poll'].includes(item.kind)" type="button" :class="buttonClass" :disabled="busy" @click="mutate(() => publish({ ...item, completed: !item.completed }))">{{ item.kind === 'poll' ? (item.completed ? 'Reopen poll' : 'Close poll') : (item.completed ? 'Mark incomplete' : 'Complete task') }}</button><span v-if="item.kind === 'task' && item.completed">✓ Completed</span><button :class="buttonClass" type="button" :disabled="busy" @click="startEdit(item)">Edit</button><button :class="buttonClass" type="button" :disabled="busy" @click="mutate(() => publish({ ...item, pinned: !item.pinned }))">{{ item.pinned ? 'Unpin' : 'Pin' }}</button><button v-if="mayDelete(item)" :class="buttonClass" type="button" :disabled="busy" :aria-label="`Delete ${item.title}`" @click="remove(item)"><Trash2 :size="14" aria-hidden="true" /></button></div>
          <button :class="buttonClass" type="button" :disabled="busy" @click="discuss(item)"><MessageSquare :size="14" aria-hidden="true" /> Discuss in conversation</button>
          <p :class="ui.subtitle">{{ item.author }}</p>
        </article>
      </div>
    </template>
  </section>
</template>

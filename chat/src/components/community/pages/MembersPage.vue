<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Menu } from "lucide-vue-next";
import spot from "@waddle/design/assets/spot-empty.svg?url";
import { css, cx } from "styled-system/css";
import { empty, seg } from "styled-system/recipes";
import { pageClasses as page } from "@/components/community/pages/page-classes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import AppDialog from "@/components/ui/AppDialog.vue";
import MemberProfileCard from "@/components/community/pages/MemberProfileCard.vue";
import { $mucCallParticipants, normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { useInCallOverlays } from "@/presence/in-call-overlay-store";
import { barePeerJid } from "@/lib/xmpp/jid";
import { buildMemberCards, isAroundPresence, type MemberCardModel } from "@/shell/controllers/use-people-rail";
import type { ChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  controller: ChatAppController;
  callParticipants?: Record<string, readonly string[]>;
}>();

const emit = defineEmits<{
  openNav: [];
  selectMember: [member: MemberCardModel | null];
}>();

const {
  connectionStore,
  xmppClient,
  messaging,
  rosterContacts,
  dmConversations,
  displayedMembers,
  roomBots,
  displayedMemberState,
  authorJidByNick,
  activeChannelRoomJid,
  activeRoomChannel,
  handleOpenDm,
} = props.controller;

const participantsStore = useStore($mucCallParticipants);
const { activeSpeakerIdentities } = useCallEngine();
const { peerInCall } = useInCallOverlays();

type Filter = "here" | "all";
const filter = ref<Filter>("here");
const selectedJid = ref<string | null>(null);

const roomActive = computed(() => !!activeRoomChannel.value);

const huddleJids = computed<ReadonlySet<string>>(() => {
  const roomJid = roomActive.value ? activeChannelRoomJid.value : null;
  if (!roomJid) return new Set();
  const participants = props.callParticipants ?? participantsStore.value;
  const nicks = participants[normalizeMucCallRoomJid(roomJid)] ?? [];
  const jids = new Set<string>();
  for (const nick of nicks) {
    const jid = authorJidByNick.value[nick];
    if (jid) jids.add(barePeerJid(jid).toLowerCase());
  }
  return jids;
});

const speakingJids = computed<ReadonlySet<string>>(() => new Set(
  [...activeSpeakerIdentities.value].map((identity) => barePeerJid(identity).toLowerCase()),
));

const cards = computed<MemberCardModel[]>(() =>
  buildMemberCards({
    roomActive: roomActive.value,
    members: displayedMembers.value,
    roomBots: roomBots.value,
    roomPresence: messaging.roomPresence.value,
    authorJidByNick: authorJidByNick.value,
    contacts: rosterContacts.contacts.value,
    conversations: dmConversations.conversations.value,
    huddleJids: huddleJids.value,
    speakingJids: speakingJids.value,
    peerInCall,
  }),
);

// Same rule as the Home hero and the people rail: in a huddle, available
// or do-not-disturb is here; away and offline are not.
function isHere(card: MemberCardModel): boolean {
  return card.status === "speaking"
    || card.status === "in-huddle"
    || isAroundPresence(card.presence);
}

// Bots have no presence, so they are never "here" and sit in their own
// section whichever filter is on.
const people = computed(() => cards.value.filter((card) => !card.bot));
const botCards = computed(() => cards.value.filter((card) => card.bot));
const hereCards = computed(() => people.value.filter(isHere));
const visibleCards = computed(() => (filter.value === "here" ? hereCards.value : people.value));
const sections = computed(() =>
  [{ title: "", cards: visibleCards.value }, { title: "Bots", cards: botCards.value }]
    .filter((section) => section.cards.length > 0),
);

/** The headline counts what the grid shows: room members with a room
 * focused, otherwise everyone known by JID (roster and DM peers). Bots
 * are listed under it, not counted. */
const headline = computed(() => {
  const count = people.value.length;
  if (roomActive.value) return `${count} member${count === 1 ? "" : "s"}`;
  return `${count} ${count === 1 ? "person" : "people"}`;
});

const kicker = computed(() => {
  if (!roomActive.value) return "Your contacts";
  const name = activeRoomChannel.value?.name ?? "this room";
  if (displayedMemberState.value === "unavailable") return `In ${name} · member list unavailable, showing who is here`;
  if (displayedMemberState.value === "loading") return `In ${name} · loading`;
  return `In ${name}`;
});

const selectedCard = computed(() =>
  selectedJid.value ? cards.value.find((card) => card.jid === selectedJid.value) ?? null : null,
);

watch(selectedCard, (card) => {
  emit("selectMember", card);
});

onBeforeUnmount(() => {
  emit("selectMember", null);
});

/** Below the lg breakpoint the shell's context column, where the desktop
 * profile lives, is `display: none` (community.css), so the profile opens
 * as a dialog there instead. Checked at selection time, not tracked. */
const profileDialogOpen = ref(false);

function contextColumnPresent(): boolean {
  return typeof window !== "undefined" && window.matchMedia("(min-width: 64rem)").matches;
}

const selectedIsSelf = computed(() => {
  const session = connectionStore.session;
  const card = selectedCard.value;
  return !!session && !!card && barePeerJid(session.jid).toLowerCase() === card.jid;
});

function selectCard(card: MemberCardModel) {
  const next = selectedJid.value === card.jid ? null : card.jid;
  selectedJid.value = next;
  profileDialogOpen.value = next !== null && !contextColumnPresent();
}

function closeProfileDialog() {
  profileDialogOpen.value = false;
  selectedJid.value = null;
}

watch(profileDialogOpen, (open) => {
  if (!open) selectedJid.value = null;
});

function messageFromProfile(jid: string) {
  closeProfileDialog();
  handleOpenDm(jid);
}

function inHuddle(card: MemberCardModel): boolean {
  return card.status === "speaking" || card.status === "in-huddle";
}

function cardLabel(card: MemberCardModel): string {
  const parts = [card.name];
  if (card.bot) parts.push("bot");
  if (card.affiliation) parts.push(card.affiliation);
  if (card.statusText) parts.push(card.statusText);
  else if (card.inCall) parts.push("in a call");
  return parts.join(", ");
}

// Members-Page board: a seg filter, a card grid (two up on phones) and a
// dashed card for bots; the selected card gets the action edge.
const filterSeg = cx(seg(), css({ _coarse: { "& > *": { height: "36px" } } }));
const emptyClass = cx(empty(), css({ py: "10", "& h2": { margin: 0, font: "700 24px/1.25 {fonts.display}", color: "fg" } }));
const sectionClass = css({ display: "flex", flexDirection: "column", gap: "3" });
const grid = css({
  display: "grid",
  gridTemplateColumns: "repeat(2, minmax(0, 1fr))",
  gap: "2.5",
  margin: 0,
  padding: 0,
  listStyle: "none",
  md: { gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))", gap: "3" },
});
const cardClass = css({
  position: "relative",
  display: "flex",
  flexDirection: "column",
  alignItems: "flex-start",
  gap: "2",
  width: "100%",
  height: "100%",
  minWidth: 0,
  padding: "3",
  borderRadius: "14px",
  border: "1px solid {colors.border.soft}",
  bg: "surface",
  color: "fg",
  font: "inherit",
  textAlign: "left",
  cursor: "pointer",
  "--ring-bg": "{colors.surface}",
  transition: "border-color {durations.fast} {easings.out}, background-color {durations.fast} {easings.out}",
  _hover: { borderColor: "border.strong" },
  '&[aria-pressed="true"]': { borderColor: "action", boxShadow: "inset 0 0 0 1px {colors.action}" },
  "&[data-bot]": { borderStyle: "dashed", borderColor: "border" },
  md: { padding: "4", borderRadius: "16px" },
});
const cardText = css({ display: "flex", flexDirection: "column", gap: "0.5", width: "100%", minWidth: 0 });
const cardName = css({ font: "700 15px/1.3 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", _narrow: { fontSize: "14px" } });
const cardMeta = css({ display: "block", font: "400 13px/1.35 {fonts.body}", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", _firstLetter: { textTransform: "uppercase" } });
const liveText = css({ color: "live.text" });
const capitalize = css({ textTransform: "capitalize" });
const dialogBody = css({ minHeight: 0, flex: 1, overflowY: "auto" });
</script>

<template>
  <div :class="page.root">
    <!-- Phone header: the rail bar with the sidebar button (Mobile-Members board). -->
    <header :class="page.mobileBar" data-theme="night">
      <button type="button" :class="page.mobileNav" aria-label="Open navigation" @click="emit('openNav')">
        <Menu aria-hidden="true" />
      </button>
      <b :class="page.mobileTitle">People</b>
    </header>

    <div :class="page.scroll">
      <div :class="page.content">
        <header :class="page.head">
          <div :class="page.heading">
            <span :class="page.kicker">{{ kicker }}</span>
            <h1 :class="page.h1">{{ headline }}</h1>
            <p :class="page.lead">
              {{ hereCards.length }} here now. Pick someone to see their profile.
            </p>
          </div>
          <div :class="filterSeg" role="group" aria-label="Filter members">
            <button type="button" :aria-pressed="filter === 'here'" @click="filter = 'here'">
              Here now
            </button>
            <button type="button" :aria-pressed="filter === 'all'" @click="filter = 'all'">
              Everyone
            </button>
          </div>
        </header>

        <div v-if="visibleCards.length === 0" :class="emptyClass" role="status">
          <img :src="spot" alt="" />
          <template v-if="filter === 'here' && people.length > 0">
            <h2>Nobody is here right now.</h2>
            <p>Switch to Everyone to see the whole list.</p>
          </template>
          <template v-else-if="roomActive">
            <h2>It is quiet in here.</h2>
            <p>Be the one who breaks the silence.</p>
          </template>
          <template v-else>
            <h2>No contacts yet.</h2>
            <p>Say hello in a room and people will show up here.</p>
          </template>
        </div>
        <section v-for="section in sections" :key="section.title" :class="sectionClass">
          <h2 v-if="section.title" :class="page.sectionTitle">{{ section.title }} · {{ section.cards.length }}</h2>
          <ul :class="grid">
            <li v-for="card in section.cards" :key="card.jid">
              <button
                type="button"
                :class="cardClass"
                :aria-pressed="selectedJid === card.jid"
                :aria-label="cardLabel(card)"
                :data-bot="card.bot ? '' : undefined"
                @click="selectCard(card)"
              >
                <UserAvatar
                  :name="card.name"
                  :jid="card.jid"
                  :presence="card.presence"
                  :in-call="card.inCall"
                  :huddle="inHuddle(card) || card.inCall"
                  :speaking="card.status === 'speaking'"
                  size="lg"
                />
                <span :class="cardText">
                  <span :class="cardName">{{ card.name }}</span>
                  <span v-if="card.affiliation || card.statusText || card.bot" :class="[cardMeta, inHuddle(card) && liveText]">
                    <template v-if="card.bot">Bot</template>
                    <template v-else>
                      <span v-if="card.affiliation" :class="capitalize">{{ card.affiliation }}</span>
                      <template v-if="card.affiliation && card.statusText"> · </template>
                      <template v-if="card.statusText">{{ card.statusText }}</template>
                    </template>
                  </span>
                </span>
              </button>
            </li>
          </ul>
        </section>
      </div>
    </div>

    <AppDialog v-model:open="profileDialogOpen">
      <div :class="dialogBody">
        <MemberProfileCard
          v-if="selectedCard"
          :member="selectedCard"
          :xmpp-client="xmppClient"
          :is-self="selectedIsSelf"
          @message="messageFromProfile"
          @close="closeProfileDialog"
        />
      </div>
    </AppDialog>
  </div>
</template>

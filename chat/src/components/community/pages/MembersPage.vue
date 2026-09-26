<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Menu } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
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
  membersWithAvatars,
  displayedMemberState,
  authorJidByNick,
  avatarUrlByAuthor,
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
    members: membersWithAvatars.value,
    roomPresence: messaging.roomPresence.value,
    authorJidByNick: authorJidByNick.value,
    avatarUrlByAuthor: avatarUrlByAuthor.value,
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

const hereCards = computed(() => cards.value.filter(isHere));
const visibleCards = computed(() => (filter.value === "here" ? hereCards.value : cards.value));

/** The headline counts what the grid shows: room members with a room
 * focused, otherwise everyone known by JID (roster and DM peers). */
const headline = computed(() => {
  const count = cards.value.length;
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

function ringClass(card: MemberCardModel): string {
  if (card.status === "speaking") return "huddle-ring huddle-ring--speaking";
  if (card.status === "in-huddle" || card.inCall) return "huddle-ring";
  return "";
}

function cardLabel(card: MemberCardModel): string {
  const parts = [card.name];
  if (card.affiliation) parts.push(card.affiliation);
  if (card.statusText) parts.push(card.statusText);
  else if (card.inCall) parts.push("in a call");
  return parts.join(", ");
}
</script>

<template>
  <div class="community-page">
    <header class="community-page__header">
      <div class="flex min-w-0 items-start gap-3">
        <button
          type="button"
          class="community-page__mobile-nav lg:hidden"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Menu class="h-4 w-4" aria-hidden="true" />
        </button>
        <div class="community-page__heading">
          <span class="community-kicker">{{ kicker }}</span>
          <h1 class="community-page__title">{{ headline }}</h1>
          <p class="community-page__lead">
            {{ hereCards.length }} here now. Pick someone to see their profile.
          </p>
        </div>
      </div>
      <div class="community-filter" role="group" aria-label="Filter members">
        <button
          type="button"
          class="community-filter__option"
          :aria-pressed="filter === 'here'"
          @click="filter = 'here'"
        >
          Here now
        </button>
        <button
          type="button"
          class="community-filter__option"
          :aria-pressed="filter === 'all'"
          @click="filter = 'all'"
        >
          Everyone
        </button>
      </div>
    </header>

    <div class="community-page__body">
      <div v-if="visibleCards.length === 0" class="community-empty">
        <template v-if="filter === 'here' && cards.length > 0">
          Nobody is here right now. Switch to Everyone to see the whole list.
        </template>
        <template v-else-if="roomActive">
          It is quiet in here. Be the one who breaks the silence.
        </template>
        <template v-else>
          No contacts yet. Say hello in a room and people will show up here.
        </template>
      </div>
      <ul v-else class="community-grid">
        <li v-for="card in visibleCards" :key="card.jid" class="contents">
        <button
          type="button"
          class="member-card"
          :aria-pressed="selectedJid === card.jid"
          :aria-label="cardLabel(card)"
          @click="selectCard(card)"
        >
          <span :class="ringClass(card)">
            <AppAvatar
              :name="card.name"
              :src="card.avatarUrl"
              :presence="card.presence"
              :in-call="card.inCall"
              size="lg"
            />
          </span>
          <span class="member-card__text">
            <span class="member-card__name">{{ card.name }}</span>
            <span class="member-card__meta">
              <span v-if="card.affiliation" class="community-kicker">{{ card.affiliation }}</span>
              <span
                v-if="card.statusText"
                :class="card.status === 'speaking' || card.status === 'in-huddle' ? 'text-live-text' : ''"
              >{{ card.statusText }}</span>
            </span>
          </span>
          <span v-if="card.status === 'speaking'" class="speaking-bars" aria-hidden="true">
            <span /><span /><span />
          </span>
        </button>
        </li>
      </ul>
    </div>

    <AppDialog v-model:open="profileDialogOpen">
      <div class="chat-pane-scroll min-h-0 flex-1 p-4">
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

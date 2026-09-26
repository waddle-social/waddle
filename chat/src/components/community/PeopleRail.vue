<script setup lang="ts">
import { computed, ref, useId } from "vue";
import { useStore } from "@nanostores/vue";
import { $mucCallParticipantOwners, $mucCallParticipants } from "@/lib/calls/muc-call-presence";
import { $dmCallActivities } from "@/lib/calls/dm-call-activity";
import { $callState } from "@/lib/calls/call-store";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { useInCallOverlays } from "@/presence/in-call-overlay-store";
import { barePeerJid } from "@/lib/xmpp/jid";
import { filterPeople, usePeopleRail } from "@/shell/controllers/use-people-rail";
import type { ChatAppController } from "@/shell/chat-app-controller";
import PeopleRailGroup from "@/components/community/PeopleRailGroup.vue";

const props = defineProps<{
  controller: ChatAppController;
  /** Muji participants keyed by room JID; defaults to the live store. */
  callParticipants?: Record<string, readonly string[]>;
}>();

const {
  connectionStore,
  ui,
  messaging,
  authorJidByNick,
  avatarUrlByAuthor,
  membersWithAvatars,
  activeChannelRoomJid,
  activeRoomChannel,
  rosterContacts,
  dmConversations,
  handleOpenDm,
} = props.controller;

const participantsStore = useStore($mucCallParticipants);
const ownersStore = useStore($mucCallParticipantOwners);
const dmCallActivitiesStore = useStore($dmCallActivities);
const callStateStore = useStore($callState);
const { activeSpeakerIdentities } = useCallEngine();
const { peerInCall } = useInCallOverlays();

const query = ref("");
const searchId = useId();

const groups = usePeopleRail({
  selfJid: computed(() => connectionStore.session?.jid ?? null),
  roomPresence: messaging.roomPresence,
  authorJidByNick,
  avatarUrlByAuthor,
  members: membersWithAvatars,
  activeRoomJid: computed(() => (activeRoomChannel.value ? activeChannelRoomJid.value : null)),
  callParticipants: computed(() => props.callParticipants ?? participantsStore.value),
  callParticipantOwners: ownersStore,
  ownCallRoomJid: computed(() => {
    const state = callStateStore.value;
    return state.phase === "active" && state.kind === "muc" ? barePeerJid(state.peer) : null;
  }),
  // LiveKit identities are full JIDs and exist only for the call we are in.
  speakingJids: computed(() => new Set(
    [...activeSpeakerIdentities.value].map((identity) => barePeerJid(identity).toLowerCase()),
  )),
  dmCallActivities: dmCallActivitiesStore,
  contacts: rosterContacts.contacts,
  conversations: dmConversations.conversations,
  peerInCall,
});

const huddle = computed(() => filterPeople(groups.value.huddle, query.value));
const room = computed(() => filterPeople(groups.value.room, query.value));
const around = computed(() => filterPeople(groups.value.around, query.value));
const awayAndOffline = computed(() => filterPeople(groups.value.awayAndOffline, query.value));
const roomTitle = computed(() => activeRoomChannel.value ? "In this room" : "");

function openPerson(jid: string) {
  ui.showMobileNav.value = false;
  void handleOpenDm(jid);
}
</script>

<template>
  <div class="people-rail">
    <div class="people-rail__search">
      <label :for="searchId" class="sr-only">Find people</label>
      <input
        :id="searchId"
        v-model="query"
        class="people-rail__input"
        type="search"
        placeholder="Find people"
        autocomplete="off"
      />
    </div>
    <div class="people-rail__scroll">
      <PeopleRailGroup
        title="In a huddle"
        :count="huddle.length"
        :people="huddle"
        live
        empty-text="Nobody is in a huddle right now. Start one."
        @select="openPerson"
      />
      <PeopleRailGroup
        v-if="roomTitle"
        :title="roomTitle"
        :count="room.length"
        :people="room"
        empty-text="It is quiet in here. Be the one who breaks the silence."
        @select="openPerson"
      />
      <PeopleRailGroup
        title="Around"
        :count="around.length"
        :people="around"
        empty-text="Nobody around right now."
        @select="openPerson"
      />
      <PeopleRailGroup
        title="Away and offline"
        :count="awayAndOffline.length"
        :people="awayAndOffline"
        collapsible
        @select="openPerson"
      />
    </div>
  </div>
</template>

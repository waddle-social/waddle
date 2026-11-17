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
import { css } from "styled-system/css";
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
  displayedMembers,
  roomBots,
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
  members: displayedMembers,
  roomBots,
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
const bots = computed(() => filterPeople(groups.value.bots, query.value));
const roomTitle = computed(() => activeRoomChannel.value ? "In this room" : "");

const rootClass = css({ display: "flex", flexDirection: "column", minHeight: 0, gap: "1" });
const searchClass = css({ padding: "0 {spacing.0.5} {spacing.1}" });
const inputClass = css({
  width: "100%",
  height: "40px",
  boxSizing: "border-box",
  px: "3",
  borderRadius: "control",
  border: "1px solid {colors.border.strong}",
  bg: "surface",
  color: "fg",
  textStyle: "input",
  _placeholder: { color: "fg.soft" },
  _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
});
const scrollClass = css({ display: "flex", flexDirection: "column", gap: "2", minHeight: 0, overflowY: "auto" });
const srOnly = css({ srOnly: true });

function openPerson(jid: string) {
  ui.showMobileNav.value = false;
  void handleOpenDm(jid);
}
</script>

<template>
  <div :class="rootClass">
    <div :class="searchClass">
      <label :for="searchId" :class="srOnly">Find people</label>
      <input
        :id="searchId"
        v-model="query"
        :class="inputClass"
        type="search"
        placeholder="Find people"
        autocomplete="off"
      />
    </div>
    <div :class="scrollClass">
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
      <PeopleRailGroup v-if="bots.length > 0" title="Bots" :count="bots.length" :people="bots" bots />
    </div>
  </div>
</template>

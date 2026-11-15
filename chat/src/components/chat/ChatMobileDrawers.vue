<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $mucCallMedia,
  $mucCallParticipants,
  mucCallParticipantCounts,
} from "@/lib/calls/muc-call-presence";
import { $dmCallActivities } from "@/lib/calls/dm-call-activity";
import { $callState } from "@/lib/calls/call-store";
import {
  activeChannelRailCallCount,
  activeDmRailCallCount,
} from "@/lib/calls/call-rail-counts";
import { ChevronRight, PencilLine, Users } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { floe, kicker } from "styled-system/recipes";
import AppSidebar from "@/components/community/AppSidebar.vue";
import SettingsMobileHeader from "@/components/chat/SettingsMobileHeader.vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import { liveKicker } from "@/ui/part-classes";
import { extensionRouteIconComponent, extensionRouteRailItems } from "./extension-route-rail-model";
import type { CallMedia } from "@/lib/calls/types";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import type { ChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  controller: ChatAppController;
  activeChannelCallCount?: number;
  activeDmCallCount?: number;
  callParticipantCounts?: Record<string, number>;
  callParticipants?: Record<string, string[]>;
  callMediaByRoom?: Record<string, CallMedia>;
}>();

const {
  ui,
  waddles,
  memberCountLabel,
  channelExtensionRoutes,
  activeExtensionRouteKey,
  activeRightPanel,
  openCommunitySurface,
  selectExtensionRoute,
  openChannelEdit,
  openHome,
} = props.controller;

function selectCommunitySurface(surface: "feed" | "events") {
  openCommunitySurface(surface);
}

// XEP-0272 Muji participants keyed by room JID — the same retained
// reactive state ChatReadyShell derives, so the drawer's people rail
// shows the same huddle membership as the desktop rail.
const mucCallParticipantsStore = useStore($mucCallParticipants);
const mucCallMediaStore = useStore($mucCallMedia);
const dmCallActivitiesStore = useStore($dmCallActivities);
const callStateStore = useStore($callState);
const fallbackCallParticipantCounts = computed<Record<string, number>>(() => {
  return mucCallParticipantCounts(mucCallParticipantsStore.value);
});
const visibleCallParticipantCounts = computed<Record<string, number>>(() =>
  props.callParticipantCounts ?? fallbackCallParticipantCounts.value,
);
const visibleCallParticipants = computed<Record<string, string[]>>(() =>
  props.callParticipants ?? mucCallParticipantsStore.value,
);
const visibleCallMediaByRoom = computed<Record<string, CallMedia>>(() =>
  props.callMediaByRoom ?? mucCallMediaStore.value,
);
const visibleActiveChannelCallCount = computed(() => {
  return props.activeChannelCallCount ??
    activeChannelRailCallCount(visibleCallParticipantCounts.value, callStateStore.value);
});
const visibleActiveDmCallCount = computed(() => {
  return props.activeDmCallCount ??
    activeDmRailCallCount(dmCallActivitiesStore.value, callStateStore.value);
});
/** Live-call summary for the drawer header: the desktop header's
 * "Start a huddle" affordance is not in the drawer, so name what is live. */
const liveSummary = computed(() => {
  const parts: string[] = [];
  const rooms = visibleActiveChannelCallCount.value;
  const dms = visibleActiveDmCallCount.value;
  if (rooms > 0) parts.push(`${rooms} room huddle${rooms === 1 ? "" : "s"}`);
  if (dms > 0) parts.push(`${dms} call${dms === 1 ? "" : "s"}`);
  const media = Object.values(visibleCallMediaByRoom.value).some((entry) => entry.video) ? " with video" : "";
  return parts.length > 0 ? `${parts.join(" · ")}${media}` : "";
});

const drawerExtensionRoutes = computed(() =>
  extensionRouteRailItems(
    channelExtensionRoutes.value,
    activeExtensionRouteKey.value,
    activeRightPanel.value === "extension",
  ),
);

const navWidthClass = css({ width: "min(88vw, 320px)" });
const liveRowClass = cx(liveKicker, css({ display: "flex", alignItems: "center", gap: "2", padding: "14px 12px 4px" }));
const detailsClass = css({ display: "flex", flexDirection: "column", gap: "3.5", padding: "8px 12px 28px 16px" });
const spaceCardClass = cx(floe(), css({ "--pad": "14px", gap: "1", borderRadius: "14px", borderColor: "border.soft" }));
const spaceNameClass = css({ font: "700 16px/1.3 {fonts.display}" });
const captionClass = css({ textStyle: "caption", color: "fg.muted" });
const listClass = css({ display: "flex", flexDirection: "column", gap: "0.5" });
const rowClass = css({
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  minHeight: "48px",
  px: "3",
  border: 0,
  borderRadius: "control",
  bg: "transparent",
  color: "fg",
  font: "600 15px/1.3 {fonts.body}",
  textAlign: "left",
  cursor: "pointer",
  _hover: { bg: "state.hover" },
  _currentPage: { bg: "state.selected" },
  "& svg": { width: "20px", height: "20px", flex: "none", color: "fg.muted" },
});
const growClass = css({ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const sectionClass = cx(kicker(), css({ margin: 0, padding: "6px 4px 0" }));

function openExtensionRoute(route: DiscoveredExtensionRoute) {
  const channel = waddles.currentChannel.value;
  if (!channel) return;
  ui.showMobileDetails.value = false;
  void selectExtensionRoute(channel.id, route);
}
</script>

<template>
    <!-- Mobile header (settings page only - chat pages render the consolidated header inside ContentArea).
         Phone-only, so its sidebar button goes back to the home screen, which is the sidebar. -->
    <SettingsMobileHeader
      v-if="ui.activePage.value === 'settings'"
      @open-nav="openHome()"
    />

    <!-- Mobile navigation: the sidebar itself (Mobile-Home board). -->
    <AppDrawer
      v-model:open="ui.showMobileNav.value"
      side="left"
      label="Navigation drawer"
      :width-class="navWidthClass"
      bare
    >
      <AppSidebar :controller="controller">
        <template v-if="liveSummary" #live>
          <span :class="liveRowClass"><span class="wd-live-dot" aria-hidden="true" />{{ liveSummary }}</span>
        </template>
      </AppSidebar>
    </AppDrawer>

    <!-- Mobile details drawer (Mobile-Details board) -->
    <AppDrawer v-model:open="ui.showMobileDetails.value" side="right" label="Details drawer">
      <template #title>Details</template>
      <div :class="detailsClass">
        <div v-if="waddles.currentSpace.value" :class="spaceCardClass">
          <span :class="kicker()">Space</span>
          <b :class="spaceNameClass">{{ waddles.currentSpace.value.name }}</b>
          <span v-if="waddles.currentSpace.value.description" :class="captionClass">
            {{ waddles.currentSpace.value.description }}
          </span>
        </div>

        <div :class="listClass">
          <button
            v-if="waddles.currentChannel.value && waddles.canManageChannels.value"
            :class="rowClass"
            type="button"
            @click="openChannelEdit(); ui.showMobileDetails.value = false"
          >
            <PencilLine aria-hidden="true" />
            <span>Edit room</span>
          </button>
          <button
            :class="rowClass"
            type="button"
            @click="ui.showMobileDetails.value = false; ui.showMembers.value = true"
          >
            <Users aria-hidden="true" />
            <span :class="growClass">Members ({{ memberCountLabel }})</span>
            <ChevronRight aria-hidden="true" />
          </button>
        </div>

        <template v-if="drawerExtensionRoutes.length > 0">
          <h3 :class="sectionClass">Extensions</h3>
          <div :class="listClass">
            <button
              v-for="item in drawerExtensionRoutes"
              :key="item.key"
              type="button"
              :class="rowClass"
              :aria-current="item.isActive ? 'page' : undefined"
              @click="openExtensionRoute(item.route)"
            >
              <component :is="extensionRouteIconComponent(item.icon)" aria-hidden="true" />
              <span :class="growClass">{{ item.label }}</span>
            </button>
          </div>
        </template>
      </div>
    </AppDrawer>

</template>

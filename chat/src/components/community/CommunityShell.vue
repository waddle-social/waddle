<script setup lang="ts">
import CommunityHeader from "@/components/community/CommunityHeader.vue";
import MobileTabBar from "@/components/community/MobileTabBar.vue";
import PeopleRail from "@/components/community/PeopleRail.vue";
import type { ChatAppController } from "@/shell/chat-app-controller";

/**
 * Huddle community shell: a 60px header with pill navigation, the
 * always-present people rail on the left, the page in the middle, an
 * optional context column on the right (`#context` slot) and, on phones,
 * a bottom tab bar. The page itself is the default slot.
 */
defineProps<{
  controller: ChatAppController;
  /** Settings (and admin, which bypasses the shell) render without the rail. */
  hideRail?: boolean;
  canSearch?: boolean;
  openSearch?: () => void;
  startHuddle: () => void;
  callParticipants?: Record<string, readonly string[]>;
}>();
</script>

<template>
  <div class="community-shell">
    <CommunityHeader
      :controller="controller"
      :can-search="canSearch"
      :open-search="openSearch"
      :start-huddle="startHuddle"
    />
    <div class="community-shell__body">
      <aside v-if="!hideRail" class="community-shell__rail" aria-label="People">
        <PeopleRail :controller="controller" :call-participants="callParticipants" />
      </aside>
      <main class="community-shell__main">
        <slot />
      </main>
      <aside v-if="$slots.context" class="community-shell__context" aria-label="Context">
        <slot name="context" />
      </aside>
    </div>
    <MobileTabBar :controller="controller" />
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import ChatReadyShell from "@/components/chat/ChatReadyShell.vue";
import { settleIslandMatch, type RouteMatch } from "@/router";
import { appController } from "@/stores/app-controller";

// One Vue island per Astro route. The route id is provided by the
// Astro page; when the router's match disagrees (Astro accepted a URL
// the route parser rejects, so the match fell back to home) the URL is
// replaced with the match's canonical href instead of asserting, and
// the shell renders whatever the match says. All routes currently
// delegate rendering to `ChatReadyShell`; future per-route extraction
// can grow specific pages alongside this fallback without changing the
// routing layer.
const props = defineProps<{
  routeId: RouteMatch["id"];
}>();

settleIslandMatch(props.routeId);

const controller = computed(() => appController.value);
</script>

<template>
  <ChatReadyShell v-if="controller" :controller="controller" />
</template>

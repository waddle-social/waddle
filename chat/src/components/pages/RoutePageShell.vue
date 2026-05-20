<script setup lang="ts">
import { computed } from "vue";
import ChatReadyShell from "@/components/chat/ChatReadyShell.vue";
import { useTypedMatch, type RouteMatch } from "@/router";
import { appController } from "@/stores/app-controller";

// One Vue island per Astro route. The route id is provided by the
// Astro page so `useTypedMatch` can assert (eagerly at setup, and
// reactively on history-driven drift) that this island is mounted
// on the right route. All routes currently delegate rendering to
// `ChatReadyShell`; future per-route extraction can grow specific
// pages alongside this fallback without changing the routing layer.
const props = defineProps<{
  routeId: RouteMatch["id"];
  initialMatch?: RouteMatch;
}>();

useTypedMatch(props.routeId, props.initialMatch);

const controller = computed(() => appController.value);
</script>

<template>
  <ChatReadyShell v-if="controller" :controller="controller" />
</template>

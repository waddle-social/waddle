<script setup lang="ts">
import { computed } from "vue";
import ChatReadyShell from "@/components/chat/ChatReadyShell.vue";
import { useTypedMatch, type RouteMatch } from "@/router";
import { appController } from "@/stores/app-controller";

const props = defineProps<{ initialMatch?: RouteMatch }>();

useTypedMatch("admin", props.initialMatch);

const controller = computed(() => appController.value);
</script>

<template>
  <ChatReadyShell v-if="controller" :controller="controller" />
</template>

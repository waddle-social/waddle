<script setup lang="ts">
import { css, cx } from "styled-system/css";
import { seg } from "styled-system/recipes";
import { buildHref } from "@/router";
import { appController } from "@/stores/app-controller";

/**
 * Inbox switch (App-Unread / App-Discussions boards): Unread and
 * Discussions are two routes under one Inbox page. Real links, so they
 * open in a new tab; a plain click navigates in place through the shell.
 */
const props = defineProps<{ current: "unread" | "threads" }>();

const navClass = cx(seg(), css({ alignSelf: "flex-start", _coarse: { "& > *": { height: "38px" } } }));

const tabs = [
  { id: "unread", label: "Unread", href: buildHref({ id: "unread" }) },
  { id: "threads", label: "Discussions", href: buildHref({ id: "threads" }) },
] as const;

function go(event: MouseEvent, id: "unread" | "threads") {
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
  event.preventDefault();
  if (id === props.current) return;
  const c = appController.value;
  if (id === "unread") c?.openUnread();
  else c?.openThreads();
}
</script>

<template>
  <nav aria-label="Inbox" :class="navClass">
    <a
      v-for="tab in tabs"
      :key="tab.id"
      :href="tab.href"
      :aria-current="tab.id === current ? 'page' : undefined"
      @click="go($event, tab.id)"
    >{{ tab.label }}</a>
  </nav>
</template>

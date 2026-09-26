<script setup lang="ts">
import { computed } from "vue";
import { CalendarDays, Home, LayoutGrid, MessagesSquare, Users } from "lucide-vue-next";
import { buildHref, type RouteMatch } from "@/router";
import type { ChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  controller: ChatAppController;
}>();

const { ui, openHome, openRooms, openThreads, openCommunitySurface } = props.controller;

type TabId = "home" | "rooms" | "threads" | "events";

const tabs: { id: TabId; label: string; href: string; icon: typeof Home; go: () => void }[] = [
  { id: "home", label: "Home", href: buildHref({ id: "home" } as RouteMatch), icon: Home, go: () => openHome() },
  { id: "rooms", label: "Rooms", href: buildHref({ id: "rooms" } as RouteMatch), icon: LayoutGrid, go: () => openRooms() },
  { id: "threads", label: "Discuss", href: buildHref({ id: "threads" } as RouteMatch), icon: MessagesSquare, go: () => openThreads() },
  { id: "events", label: "Events", href: buildHref({ id: "events" } as RouteMatch), icon: CalendarDays, go: () => openCommunitySurface("events") },
];

const activeTab = computed<TabId | null>(() => {
  if (ui.activeCommunitySurface.value === "events") return "events";
  if (ui.activeCommunitySurface.value === "feed") return null;
  switch (ui.activePage.value) {
    case "dashboard":
      return "home";
    case "rooms":
    case "chat":
      return "rooms";
    case "threads":
    case "unread":
      return "threads";
    default:
      return null;
  }
});

function openPeople() {
  ui.showMobileNav.value = true;
}
</script>

<template>
  <nav class="community-tabbar" aria-label="Primary">
    <a
      :href="tabs[0]!.href"
      class="community-tabbar__tab"
      :aria-current="activeTab === 'home' ? 'page' : undefined"
      @click.prevent="tabs[0]!.go()"
    >
      <Home class="community-tabbar__icon" aria-hidden="true" />
      <span>Home</span>
    </a>
    <button
      type="button"
      class="community-tabbar__tab"
      :aria-expanded="ui.showMobileNav.value"
      @click="openPeople"
    >
      <Users class="community-tabbar__icon" aria-hidden="true" />
      <span>People</span>
    </button>
    <a
      v-for="tab in tabs.slice(1)"
      :key="tab.id"
      :href="tab.href"
      class="community-tabbar__tab"
      :aria-current="activeTab === tab.id ? 'page' : undefined"
      @click.prevent="tab.go()"
    >
      <component :is="tab.icon" class="community-tabbar__icon" aria-hidden="true" />
      <span>{{ tab.label }}</span>
    </a>
  </nav>
</template>

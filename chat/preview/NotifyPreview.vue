<script setup lang="ts">
import NotifyModeButton from "@/components/chat/NotifyModeButton.vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { NotifySettingsStore } from "@/lib/notify-settings";

/** screen=notify: the bell menu over the room, with a client stub whose publishes never settle (Saving…) or fail. */
defineProps<{ store: NotifySettingsStore }>();
const client = {} as BrowserXmppClient;
</script>

<template>
  <div style="position: fixed; top: 64px; right: 200px; z-index: 50">
    <NotifyModeButton room-jid="general@muc.waddle.test" conversation-kind="public-group" room-name="general" :client="client" :store="store" />
  </div>
</template>

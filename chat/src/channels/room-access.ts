import { computed, ref, watch, type ComputedRef, type Ref } from "vue";
import {
  bareJidKey,
  type BrowserXmppClient,
  type RoomAccessChangedEvent,
} from "@/lib/xmpp-client";

export type ChannelLoadIntent = "automatic" | "explicit-navigation";

type RequiredRoomAccess = Pick<
  Extract<RoomAccessChangedEvent, { state: "required" }>,
  "roomJid" | "condition"
>;

export function useRoomAccessRequirements(
  xmppClient: Ref<BrowserXmppClient | null>,
  currentRoomJid: ComputedRef<string | null>,
) {
  const requirements = ref<Record<string, RequiredRoomAccess>>({});
  const currentRoomAccessRequirement = computed(() => {
    const roomJid = currentRoomJid.value;
    return roomJid ? requirements.value[bareJidKey(roomJid)] ?? null : null;
  });

  function applyEvent(event: RoomAccessChangedEvent) {
    const key = bareJidKey(event.roomJid);
    if (event.state === "required") {
      requirements.value = {
        ...requirements.value,
        [key]: {
          roomJid: event.roomJid,
          condition: event.condition,
        },
      };
      return;
    }
    const next = { ...requirements.value };
    delete next[key];
    requirements.value = next;
  }

  function isRoomAccessRequired(roomJid: string): boolean {
    return !!requirements.value[bareJidKey(roomJid)];
  }

  watch(xmppClient, (client, _previousClient, onCleanup) => {
    requirements.value = {};
    if (!client) return;

    const unsubscribe = client.onRoomAccessChanged?.(applyEvent) ?? (() => {});
    onCleanup(unsubscribe);
    for (const requirement of client.listRoomAccessRequirements?.() ?? []) {
      applyEvent(requirement);
    }
  }, { immediate: true });

  return {
    currentRoomAccessRequirement,
    isRoomAccessRequired,
  };
}

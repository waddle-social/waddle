import { describe, expect, test } from "bun:test";
import { computed, ref } from "vue";
import { useSendOrchestration } from "../src/shell/controllers/use-send-orchestration";

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
}

describe("retryActiveLoad", () => {
  test("explicit DM retry recovers a superseded session before reloading", async () => {
    const calls: string[] = [];
    const xmppClient = ref({
      async recoverSupersededSession() {
        calls.push("recover");
      },
    });
    const dmMessaging = {
      async loadMessages(peerJid: string) {
        calls.push(`load:${peerJid}`);
      },
    };

    const { retryActiveLoad } = useSendOrchestration({
      ui: {} as never,
      xmppClient: computed(() => xmppClient.value as never),
      waddles: {} as never,
      messaging: {} as never,
      dmMessaging: dmMessaging as never,
      isActiveDirectDmSurface: () => true,
      activeTarget: computed(() => dmMessaging as never),
      activeDmPeer: computed(() => ({ peerJid: "bob@example.com" })),
    });

    retryActiveLoad();
    await flushMicrotasks();

    expect(calls).toEqual(["recover", "load:bob@example.com"]);
  });

  test("non-DM retry stays inert", async () => {
    let recovered = false;
    let loaded = false;
    const xmppClient = ref({
      async recoverSupersededSession() {
        recovered = true;
      },
    });
    const dmMessaging = {
      async loadMessages() {
        loaded = true;
      },
    };

    const { retryActiveLoad } = useSendOrchestration({
      ui: {} as never,
      xmppClient: computed(() => xmppClient.value as never),
      waddles: {} as never,
      messaging: {} as never,
      dmMessaging: dmMessaging as never,
      isActiveDirectDmSurface: () => false,
      activeTarget: computed(() => dmMessaging as never),
      activeDmPeer: computed(() => ({ peerJid: "bob@example.com" })),
    });

    retryActiveLoad();
    await flushMicrotasks();

    expect(recovered).toBe(false);
    expect(loaded).toBe(false);
  });

  test("revalidates the active DM after recovery before reloading", async () => {
    const calls: string[] = [];
    const activeSurface = ref(true);
    const activePeer = ref<{ peerJid: string } | null>({ peerJid: "alice@example.com" });
    const xmppClient = ref({
      async recoverSupersededSession() {
        calls.push("recover");
        activePeer.value = { peerJid: "bob@example.com" };
      },
    });
    const dmMessaging = {
      async loadMessages(peerJid: string) {
        calls.push(`load:${peerJid}`);
      },
    };

    const { retryActiveLoad } = useSendOrchestration({
      ui: {} as never,
      xmppClient: computed(() => xmppClient.value as never),
      waddles: {} as never,
      messaging: {} as never,
      dmMessaging: dmMessaging as never,
      isActiveDirectDmSurface: () => activeSurface.value,
      activeTarget: computed(() => dmMessaging as never),
      activeDmPeer: computed(() => activePeer.value),
    });

    retryActiveLoad();
    await flushMicrotasks();

    expect(calls).toEqual(["recover"]);
  });
});

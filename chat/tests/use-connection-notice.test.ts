import { describe, expect, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import { useConnectionNotice } from "../src/components/chat/composables/use-connection-notice";
import type { XmppStatusSnapshot } from "../src/lib/xmpp-client";

function makeHarness(initialState: XmppStatusSnapshot["state"] = "online") {
  const status = ref<XmppStatusSnapshot>({ state: initialState, detail: "" });
  const queued = ref(0);
  const scope = effectScope();
  let api: ReturnType<typeof useConnectionNotice> | undefined;
  scope.run(() => {
    api = useConnectionNotice({
      status: () => status.value,
      queuedMessageCount: () => queued.value,
    });
  });
  if (!api) throw new Error("composable did not initialize");
  return { api, status, queued, stop: () => scope.stop() };
}

describe("useConnectionNotice", () => {
  test("no banner while online without a recent reconnect", () => {
    const { api, stop } = makeHarness("online");
    expect(api.connectionNotice.value).toBeNull();
    expect(api.connectionStatusClasses.value).toBeNull();
    stop();
  });

  test("offline / reconnecting / error map to their tones and classes", async () => {
    const { api, status, stop } = makeHarness("online");

    status.value = { state: "offline", detail: "" };
    await nextTick();
    expect(api.connectionNotice.value?.tone).toBe("offline");
    expect(api.connectionStatusClasses.value?.chip).toContain("bg-muted/25");

    status.value = { state: "reconnecting", detail: "" };
    await nextTick();
    expect(api.connectionNotice.value?.tone).toBe("reconnecting");
    expect(api.connectionStatusClasses.value?.chip).toContain("chat-connection-chip-glow--warning");

    status.value = { state: "error", detail: "session expired" };
    await nextTick();
    expect(api.connectionNotice.value?.tone).toBe("error");
    expect(api.connectionStatusClasses.value?.chip).toContain("chat-connection-chip-glow--destructive");
    stop();
  });

  test("returning online after an outage briefly celebrates the reconnect", async () => {
    const { api, status, stop } = makeHarness("online");
    status.value = { state: "reconnecting", detail: "" };
    await nextTick();
    status.value = { state: "online", detail: "" };
    await nextTick();
    expect(api.connectionNotice.value?.tone).toBe("reconnected");
    expect(api.connectionStatusClasses.value?.chip).toContain("chat-connection-chip-glow--primary");

    api.clearReconnectedNotice();
    await nextTick();
    expect(api.connectionNotice.value).toBeNull();
    stop();
  });

  test("first-ever connect does not celebrate a reconnect", async () => {
    const { api, status, stop } = makeHarness("offline");
    status.value = { state: "online", detail: "" };
    await nextTick();
    expect(api.connectionNotice.value).toBeNull();
    api.clearReconnectedNotice();
    stop();
  });

  test("dropping offline cancels an active reconnected celebration", async () => {
    const { api, status, stop } = makeHarness("online");
    status.value = { state: "reconnecting", detail: "" };
    await nextTick();
    status.value = { state: "online", detail: "" };
    await nextTick();
    expect(api.connectionNotice.value?.tone).toBe("reconnected");

    status.value = { state: "offline", detail: "" };
    await nextTick();
    expect(api.connectionNotice.value?.tone).toBe("offline");
    api.clearReconnectedNotice();
    stop();
  });
});

<script setup lang="ts">
import { onBeforeUnmount, onMounted, watch } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callState,
  reportCallError,
  tearDownActiveCall,
} from "@/lib/calls/call-store";
import type { CallWireSender } from "@/lib/calls/outbound";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { connectionStore } from "@/lib/connection-store";
import {
  $callConnecting,
  installCallPagehideSuspension,
  resetCallControls,
} from "@/lib/calls/call-controls";
import { $callUiMode } from "@/lib/calls/ui-mode";

/**
 * Engine lifecycle host for the call subsystem.
 *
 * Previously this component owned the entire call UI surface (docked
 * panel + floating window + minimized chip + PIP). The presentation
 * has been split into per-channel surfaces mounted inside
 * `ContentArea.vue`:
 *   - `CallSplitContainer.vue` — inline split view.
 *   - `CallExpandedSurface.vue` — fills the chat content pane.
 *
 * What stays here: the LiveKit connect/disconnect plumbing tied to
 * `$callState` transitions, and the pagehide cleanup hook. The mic /
 * cam / hangup plumbing moved into `call-controls.ts` (nanostore
 * atoms + control functions) so every surface reads one source of
 * truth. This component renders nothing visible.
 */
const state = useStore($callState);
const { engine } = useCallEngine();
let disconnectPagehideSuspension: (() => void) | null = null;

function getSender(): CallWireSender | null {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as CallWireSender | undefined) ?? null;
}

// Connect to LiveKit when a fresh `active` phase appears with a new
// sid; disconnect on the way out. We key the watcher on the sid as a
// PRIMITIVE — building an object literal inside the source would
// re-fire on unrelated `$callState.set` updates (e.g. setting
// `selfNick` mid-teardown) and tear down the engine right after it
// connected. Keying on `sid` alone means a fresh call reconnects;
// status pokes don't.
watch(
  () => (state.value.phase === "active" ? state.value.sid : null),
  async (sid, _prev, onCleanup) => {
    if (!sid) return;
    const active = state.value;
    if (active.phase !== "active" || active.sid !== sid) return;
    $callConnecting.set(true);
    try {
      await engine.connect(active.join, active.media);
      resetCallControls(active.media.audio, active.media.video);
    } catch (err) {
      // The server already verified the JWT before forwarding the
      // transport, so the most common failure here is the browser
      // denying mic/cam permissions. Run the full hangup path so
      // SFU/session/Muji presence state rolls back before the local
      // slot disappears.
      await tearDownActiveCall(getSender(), "gone");
      await engine.disconnect();
      reportCallError(err);
    } finally {
      $callConnecting.set(false);
    }
    onCleanup(() => {
      void engine.disconnect();
      // Fresh call starts in split mode — expanded is per-call, not
      // a sticky preference. Otherwise hanging up an expanded call
      // would leave the next call's split surface invisible because
      // the mode was still "expanded".
      if ($callUiMode.get() === "expanded") {
        $callUiMode.set("split");
      }
    });
  },
  { immediate: true },
);

onMounted(() => {
  if (typeof window !== "undefined") {
    disconnectPagehideSuspension = installCallPagehideSuspension(window);
  }
});

onBeforeUnmount(() => {
  // Component lifecycle is not a call lifecycle. Route changes and
  // browser refreshes must not emit XMPP call-ending stanzas; explicit
  // hangup/logout own that path.
  void engine.disconnect();
  disconnectPagehideSuspension?.();
  disconnectPagehideSuspension = null;
});
</script>

<template>
  <!--
    Lifecycle host only — the visual surfaces (split + expanded) are
    mounted inside `ContentArea.vue`, so this component renders nothing.
  -->
</template>

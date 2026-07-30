<script setup lang="ts">
import { onBeforeUnmount, watch } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callState,
  reportCallError,
  tearDownActiveCall,
} from "@/lib/calls/call-store";
import type { CallWireSender } from "@/lib/calls/outbound";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { adoptCallCorrelationId, clearCallCorrelationId } from "@/lib/calls/call-correlation";
import { iceServersFromExternalServices } from "@/lib/calls/ice-servers";
import { connectionStore } from "@/lib/connection-store";
import {
  $callConnecting,
  resetCallControls,
  seedCallControlsFromEngine,
} from "@/lib/calls/call-controls";
import { $callUiMode, resetCallUiModeAfterCallEnd } from "@/lib/calls/ui-mode";
import { resetCallViewState } from "@/lib/calls/call-view-state";

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
 * `$callState` transitions. The XMPP provider owns page lifecycle cleanup. The mic /
 * cam / hangup plumbing moved into `call-controls.ts` (nanostore
 * atoms + control functions) so every surface reads one source of
 * truth. This component renders nothing visible.
 */
const state = useStore($callState);
const { engine } = useCallEngine();

function getSender(): CallWireSender | null {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as CallWireSender | undefined) ?? null;
}

// XEP-0215: ask our own server for its advertised TURN/STUN before connecting
// so LiveKit negotiates ICE over the XMPP-native, client-controlled path.
// Resolves to `[]` on any failure (the wrapper never throws) — the engine then
// connects with LiveKit's default servers instead of an empty list.
async function resolveIceServers(): Promise<RTCIceServer[]> {
  const client = connectionStore.client;
  if (!client) return [];
  return iceServersFromExternalServices(await client.fetchExternalServices());
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
    // Optimistic baseline shown while connecting; also clears any media
    // notice left over from the previous call.
    resetCallControls(active.media.audio, active.media.video);
    // Stage presentation starts each call in Gallery with nothing pinned —
    // a previous call's Speaker view or pin must not leak across.
    resetCallViewState();
    try {
      const iceServers = await resolveIceServers();
      // The ICE fetch can block for up to 10s. Re-confirm this is still the
      // same active call before connecting — a hang-up + redial during the
      // fetch would otherwise open a LiveKit session against a stale join/sid.
      const current = state.value;
      if (current.phase !== "active" || current.sid !== sid) return;
      // Adopt the call correlation id BEFORE the connect attempt, not on
      // the connected event alone: a failed join must still stamp its
      // lifecycle/failure events with the call's id (#1452). The promise
      // is kept so the failure path can await the adoption — a fast
      // connect failure must not emit its telemetry before the id lands.
      const correlationAdopted = adoptCallCorrelationId(current.join.room);
      await engine.connect(current.join, { ...current.media, iceServers });
      // Capture is best-effort inside connect(): a missing device or a
      // denied mic/cam permission no longer throws — the user joins as a
      // receive-only participant and the engine emits `mediaDevicesError`
      // (surfaced as a non-blocking notice). Reconcile the toggle UI to
      // what ACTUALLY published rather than the requested media.
      seedCallControlsFromEngine(engine);
    } catch (err) {
      // Only GENUINE join failures reach here now — failures that happen
      // BEFORE or DURING room.connect(): an invalid LiveKit grant, or a
      // dead/rejected transport. Capture (mic/cam) runs AFTER connect and
      // is best-effort, so "no device / permission denied" is no longer
      // fatal — it surfaces as the non-blocking media notice instead and
      // never reaches this catch. Run the full hangup path so
      // SFU/session/Muji presence state rolls back before the slot goes.
      // Await the in-flight adoption first: clearing (below) bumps the
      // epoch, and the failure telemetry emitted here must carry the id.
      await correlationAdopted;
      // Media first, protocol second (#1446): release any capture the
      // partial join grabbed, with the bounded wire teardown running
      // concurrently in the background — neither stalls the other.
      const engineDisconnected = engine.disconnect();
      void tearDownActiveCall(getSender(), "gone").catch(reportCallError);
      await engineDisconnected.catch(reportCallError);
      reportCallError(err);
      // A connect() failure before a room exists never emits the engine's
      // `disconnected` event, so the disconnected-handler cleanup does not
      // run — clear the correlation id here (after the failure telemetry
      // above, which must still carry it) or it would stamp later calls.
      clearCallCorrelationId();
    } finally {
      $callConnecting.set(false);
    }
    onCleanup(() => {
      void engine.disconnect();
      // Fresh call starts in split mode — expanded is per-call, not
      // a sticky preference. Otherwise hanging up an expanded call
      // would leave the next call's split surface invisible because
      // the mode was still "expanded".
      if ($callUiMode.get() !== "split") {
        $callUiMode.set(resetCallUiModeAfterCallEnd());
      }
    });
  },
  { immediate: true },
);

onBeforeUnmount(() => {
  // Component lifecycle is not a call lifecycle. Route changes and
  // browser refreshes must not emit XMPP call-ending stanzas; explicit
  // hangup/logout own that path.
  void engine.disconnect();
});
</script>

<template>
  <!--
    Lifecycle host only — the visual surfaces (split + expanded) are
    mounted inside `ContentArea.vue`, so this component renders nothing.
  -->
</template>

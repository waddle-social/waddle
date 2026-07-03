import { type ComputedRef, onMounted, onUnmounted, watch } from "vue";
import { useStore } from "@nanostores/vue";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useXmppRosterContacts } from "@/contacts/roster";
import { IdleTracker } from "@/presence/idle-tracker";
import { PresenceRegistry } from "@/presence/presence-registry";
import { $ownNotificationsSuppressed, $presenceMode, resetPresenceMode } from "@/presence/presence-store";
import { activityOverlayUpdate } from "@/presence/in-call-activity";
import {
  presenceModeFromWire,
  presenceModeToWire,
  statusPreferenceUpdate,
} from "@/presence/status-preference";
import { StatusPreferenceCoordinator } from "@/presence/status-preference-coordinator";
import {
  clearInCallOverlay,
  resetInCallOverlays,
  setInCallOverlay,
} from "@/presence/in-call-overlay-store";
import { $callState } from "@/lib/calls/call-store";
import { ActivityCoordinator, activityFlushForDisconnect } from "@/presence/activity-coordinator";
import { $manualActivity, resetManualActivity } from "@/presence/self-activity";
import type { BrowserXmppClient, PresenceUpdateEvent, PubsubEvent, SessionLifecycleEvent } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";

interface PresenceSyncDeps {
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  session: ComputedRef<WaddleSession | null>;
  dmConversations: Pick<ReturnType<typeof useDirectMessageConversations>, "updatePresence">;
  rosterContacts: Pick<ReturnType<typeof useXmppRosterContacts>, "updatePresence">;
}

/**
 * Own-presence + contact-presence orchestration (ADR-010): per-device
 * auto-away, the XEP-0108 activity node, cross-device manual-status sync,
 * per-contact resource aggregation, and the in-call overlay receive side.
 * The connection lifecycle composable feeds inbound events in and invokes
 * the session-ready / teardown hooks at the right points.
 */
export function usePresenceSync(deps: PresenceSyncDeps) {
  const { xmppClient, session, dmConversations, rosterContacts } = deps;

  // Aggregates each contact's resources into one rendered Show
  // (most-available-wins, ADR-010); inbound presence is per-resource.
  const presenceRegistry = new PresenceRegistry();
  // This device's own mode drives the Show we broadcast and the local
  // DND checks; the picker writes it via the presence store.
  const presenceMode = useStore($presenceMode);
  // Presence-DND silences this device's own message banners + sounds
  // (ADR-010 Phase 5a). Derived from the picker store so Reset-to-automatic
  // restores notifications live; badge / unread counts are unaffected, and
  // this is orthogonal to the server-side `urn:waddle:dnd:0` quiet-hours
  // schedule (both can suppress, for different reasons).
  const ownNotificationsSuppressed = useStore($ownNotificationsSuppressed);
  const isSelfDoNotDisturb = (): boolean => ownNotificationsSuppressed.value;
  // Per-device auto-away (ADR-010 Phase 2): the tracker watches in-tab
  // interaction + visibility and broadcasts Away/Extended Away with an
  // XEP-0319 idle stamp while Automatic; a Manual status suspends it.
  const idleTracker = new IdleTracker({
    now: () => Date.now(),
    getMode: () => presenceMode.value,
    onBroadcast: (presence) => {
      xmppClient.value?.setPresence(presence.show, presence.idleSince).catch(() => undefined);
    },
    // In-call pauses auto-away (ADR-010): never broadcast "Away + in a call".
    // `$callState` is the unified 1:1 + MUC store; `active` is the in-call phase.
    isPaused: () => $callState.get().phase === "active",
  });
  watch(presenceMode, () => {
    // A pick — including "Reset to automatic" — is itself an interaction:
    // restart the idle clock (so auto-away measures from now), then re-emit
    // through the tracker so it sends the new effective Show *and* commits its
    // baseline. Routing the send through `rebroadcast` (not a bare setPresence)
    // keeps the wire and the tracker's `lastEmitted` in sync, so a later
    // restore is never wrongly suppressed. A pick while disconnected rejects
    // (setPresence is unqueued) and is swallowed in onBroadcast; the
    // session-ready handler re-broadcasts on the next connect.
    idleTracker.markActive();
    idleTracker.rebroadcast();
  });
  // Re-evaluate the moment a call starts/ends so the pause is prompt: starting a
  // call restores Available immediately (rather than after the next poll), and
  // ending one resumes the idle timer from call-end.
  const callState = useStore($callState);
  watch(() => callState.value.phase === "active", () => idleTracker.evaluate());
  // Sole owner of the user's XEP-0108 activity node (ADR-010 Phase 3). The node
  // holds one item, so the automatic in-call overlay (`talking/on_the_phone` /
  // `on_video_phone`) and the user's manual activity (Settings / feed composer)
  // must not clobber each other: a live call overrides what's published, then
  // the manual activity is restored on leave. The publish/retract sinks report
  // whether they reached the wire, so a write lost while disconnected retries on
  // the next reconcile (call/manual change or session-ready) — no stale overlay.
  const activityCoordinator = new ActivityCoordinator({
    publish: (activity) => {
      const client = xmppClient.value;
      if (!client) return false;
      client.publishActivity(activity).catch(() => undefined);
      return true;
    },
    retract: () => {
      const client = xmppClient.value;
      if (!client) return false;
      client.retractActivity().catch(() => undefined);
      return true;
    },
    callState: () => $callState.get(),
    manualActivity: () => $manualActivity.get(),
  });
  // Reconcile on either trigger via the nanostores (not a Vue `watch`): the
  // subscriptions fire synchronously, so a manual publish reaches the wire
  // before the feed composer's follow-up refresh fetch — a microtask-deferred
  // Vue watcher would let the refresh race ahead of the publish.
  $callState.subscribe(() => activityCoordinator.reconcile());
  $manualActivity.subscribe(() => activityCoordinator.reconcile());
  // Cross-device manual-status sync (ADR-010 Phase 4): publish the picked mode
  // to the private `urn:waddle:status-preference:0` PEP node so it follows the
  // user across devices and survives reconnects. Auto-away stays per-device and
  // is NOT synced (it is never written here — only the chosen mode is).
  const statusPreferenceCoordinator = new StatusPreferenceCoordinator({
    // Resolve `true` only when the publish is confirmed on the wire. A
    // not-yet-ready session makes `publishStatusPreference` reject (via
    // `requireConnectedXmpp`); returning `false` then keeps the coordinator's
    // baseline unadvanced so the pick is retried on the next reconcile.
    publish: async (mode) => {
      const client = xmppClient.value;
      if (!client) return false;
      try {
        await client.publishStatusPreference(presenceModeToWire(mode));
        return true;
      } catch {
        return false;
      }
    },
    mode: () => $presenceMode.get(),
    setMode: (mode) => $presenceMode.set(mode),
  });
  // Sync subscription (not a Vue `watch`): the baseline seeded by `applyRemote`
  // must already be in place when this reconcile fires, or an adopted remote
  // pick would echo back to the wire. `applyRemote` sets the baseline before
  // writing `$presenceMode`, so this reconcile no-ops for adopted picks and
  // only publishes genuine local ones.
  $presenceMode.subscribe(() => {
    statusPreferenceCoordinator.reconcile().catch(() => undefined);
  });

  /** Client torn down (logout) — drop aggregated presence so a later
   * session never inherits another account's / a stale resource's Show,
   * and drop every contact's in-call overlay so it can't bleed into the
   * next account in this tab. */
  function onClientCleared(): void {
    presenceRegistry.clear();
    resetInCallOverlays();
  }

  function handlePresenceUpdate(event: PresenceUpdateEvent): void {
    // Collapse the contact's resources into one rendered Show before it
    // reaches the per-bare conversation/roster stores; carry the matching
    // XEP-0319 idle instant so an away dot can render the idle age.
    const rendered = presenceRegistry.apply(
      event.bareJid,
      event.resource,
      event.show,
      event.idleSince,
    );
    const renderedIdle = presenceRegistry.renderedIdleFor(event.bareJid);
    dmConversations.updatePresence({ ...event, show: rendered, idleSince: renderedIdle });
    rosterContacts.updatePresence(event.bareJid, rendered);
    // Ghost-call cleanup (ADR-010): a contact whose last resource went
    // unavailable can't still be in a call — clear any overlay a crashed
    // client left published, even if its XEP-0108 retract never arrived.
    if (rendered === "offline") clearInCallOverlay(event.bareJid);
  }

  /** In-call overlay receive side (ADR-010 Phase 3): a contact's XEP-0108
   * activity arrives as an opaque `<activity/>` on the activity PEP node.
   * `talking/on_the_phone` or `on_video_phone` lights the badge; the empty
   * retraction (or any other activity) clears it. */
  function handleActivityPubsubEvent(event: PubsubEvent): void {
    const update = activityOverlayUpdate(event, session.value?.jid ?? "");
    if (update) setInCallOverlay(update.jid, update.inCall);
  }

  /** Cross-device manual-status sync receive side (ADR-010 Phase 4): a pick
   * made on another of our resources arrives as an opaque
   * `<status-preference/>` on our own status-preference node. Adopt it
   * without re-publishing (the coordinator seeds its baseline first). */
  function handleStatusPreferencePubsubEvent(event: PubsubEvent): void {
    const mode = statusPreferenceUpdate(event, session.value?.jid ?? "");
    if (mode) statusPreferenceCoordinator.applyRemote(mode);
  }

  function onSessionReady(event: SessionLifecycleEvent, client: BrowserXmppClient): void {
    // Re-broadcast this device's chosen Show on every session-ready. A
    // fresh reconnect (resume window expired / server restart) starts the
    // session with no directed presence and the client layer never
    // auto-sends one, so a manually-set Away/DND would otherwise silently
    // revert; a resumed session re-sends the same Show idempotently. This
    // also lands a pick made while disconnected (setPresence then rejects
    // unqueued) on the next session-ready. Guard the rejection in case the
    // lifecycle event fires during teardown.
    // Re-broadcast the device's *current* effective presence — a pinned
    // Manual status, or the live auto-away Show with its XEP-0319 idle stamp
    // if the user is idle right now — so a fresh reconnect restores the true
    // state rather than a stale Available. `rebroadcast` commits the tracker's
    // baseline too, so a later restore after this out-of-band send is never
    // wrongly suppressed.
    idleTracker.rebroadcast();
    // Reconcile the activity node too: a write that never reached the wire
    // (client briefly null on connect, a rejected IQ, or a retract lost when a
    // call ended during a stream drop) is retried here, so neither a stale
    // in-call overlay nor a missing manual activity survives a reconnect.
    activityCoordinator.reconcile();
    // Sync the cross-device manual status (ADR-010 Phase 4). On a *fresh*
    // session, read the node and adopt the stored pick (unless this device
    // has an unpublished local pick, which then wins). A *resumed* session is
    // gap-free, so just flush a pick made while the stream was down rather
    // than burn an items-get round-trip.
    if (event.type === "fresh") {
      statusPreferenceCoordinator
        .syncOnConnect(() => client.fetchStatusPreference().then(presenceModeFromWire))
        .catch(() => undefined);
    } else {
      statusPreferenceCoordinator.reconcile().catch(() => undefined);
    }
    if (event.type === "fresh") {
      // A fresh session may have missed contacts' `unavailable` while we
      // were gone; drop the stale per-resource map so the fresh presence
      // probes repopulate it cleanly. A resumed session is gap-free, so
      // its registry is preserved.
      presenceRegistry.clear();
      // Same gap applies to the in-call overlay: a fresh session may have
      // missed contacts' activity retracts, so drop it and let live presence
      // + PEP repopulate. A resumed session is gap-free, so it's preserved.
      resetInCallOverlays();
    }
  }

  /** Logout teardown: preserve the user's cross-device status, flush the
   * activity node gracefully, then drop every piece of per-account presence
   * state so the next account in this tab starts clean. */
  async function flushAndResetForLogout(): Promise<void> {
    // Drop the module-global presence mode and the aggregated per-resource
    // presence so the next account in this tab doesn't inherit a manual
    // Away/DND or another user's contact presence. Suspend the sync coordinator
    // FIRST: signing out is local-only — the chosen status must persist on the
    // user's other devices — so the reset-to-Automatic below must not publish
    // (which would clear the synced preference for every device).
    statusPreferenceCoordinator.suspend();
    resetPresenceMode();
    presenceRegistry.clear();
    // Graceful disconnect (ADR-010 ghost cleanup): if a call is live the wire
    // holds the in-call overlay, so restore the user's manual activity (or clear
    // it) before the stream closes — no stale "in a call" survives on the
    // server and the user's chosen status is preserved across the logout. Then
    // drop the coordinator baseline + manual activity + receiver overlays so the
    // next account starts clean.
    const disconnectClient = xmppClient.value;
    const flush = activityFlushForDisconnect(
      $callState.get().phase === "active",
      $manualActivity.get(),
    );
    if (disconnectClient) {
      if (flush.kind === "publish") {
        await disconnectClient.publishActivity(flush.activity).catch(() => undefined);
      } else if (flush.kind === "retract") {
        await disconnectClient.retractActivity().catch(() => undefined);
      }
    }
    activityCoordinator.reset();
    resetManualActivity();
    resetInCallOverlays();
  }

  onMounted(() => {
    // Reset the idle clock at mount (the user just opened the app), then wire
    // the interaction + visibility listeners and the poll timer.
    idleTracker.markActive();
    idleTracker.start();
  });

  onUnmounted(() => {
    idleTracker.stop();
  });

  return {
    isSelfDoNotDisturb,
    onClientCleared,
    handlePresenceUpdate,
    handleActivityPubsubEvent,
    handleStatusPreferencePubsubEvent,
    onSessionReady,
    flushAndResetForLogout,
  };
}

package social.waddle.android.feature.call

import android.app.ForegroundServiceStartNotAllowedException
import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.calls.CallState
import social.waddle.android.service.CallForegroundService
import social.waddle.client.ffi.WaddleJingleReason

/**
 * Application-scoped glue between the call slot and the platform:
 * raises/stops the in-call foreground service and drives the media
 * controller — connect (grant check + XEP-0215 ICE fetch + LiveKit
 * join) when the slot turns `Active`, disconnect when it leaves.
 * Lives on the app graph, NOT an activity ViewModel, so an in-call
 * media session survives activity destruction.
 */
class CallSessionController(
    private val context: Context,
    private val sessionManager: XmppSessionManager,
    val media: CallMediaController,
    private val scope: CoroutineScope,
) {
    // Written by the state collector, read by the media-connection
    // collector — separate coroutines on a multithreaded dispatcher,
    // so publication needs @Volatile (same pattern as
    // LiveKitCallMediaController.room).
    @Volatile
    private var connectedSid: String? = null

    fun start() {
        scope.launch {
            // collectLatest: a hang-up racing a slow media connect
            // cancels the connect instead of queueing behind it (the
            // controller tears its half-open Room down on cancellation).
            sessionManager.callStore.state.collectLatest { state -> onCallState(state) }
        }
        scope.launch {
            // A media-plane death without an XMPP terminate (SFU closed
            // the room, network loss past LiveKit's reconnect budget)
            // must still end the call — nothing else observes the media
            // transport, so an Active slot would otherwise hold the FGS
            // and a dead Room forever.
            media.connection.collect { connection -> onMediaConnection(connection) }
        }
    }

    private suspend fun onCallState(state: CallState) {
        when (state) {
            is CallState.Outgoing -> startCallService()
            is CallState.Active -> {
                startCallService()
                if (connectedSid != state.sid) {
                    // Session migration (XEP-0353 tie-break-2) replaces
                    // the sid while a Room may still be connected — the
                    // old media session must die before the new join.
                    disconnectMediaIfConnected()
                    connectMedia(state)
                }
            }
            is CallState.Incoming -> {
                // Migration passes through Incoming while the OLD call's
                // Room is still connected; tear it down here so the
                // microphone never outlives its call slot.
                disconnectMediaIfConnected()
                // Accepting from the ring is a foreground interaction —
                // raise the FGS now so the mic capture that starts on
                // Active is never a background-started capture.
                if (state.accepting) startCallService()
            }
            // Group-call setup is signaling-only (XEP-0272 §Joining);
            // media connects when the slot goes Active with the join.
            is CallState.MucPending -> Unit
            CallState.Idle, is CallState.Ended -> {
                disconnectMediaIfConnected()
                // Grace before stopping the FGS: a migration takeover
                // publishes Ended(old) moments before the accepting
                // ring for the new sid, and Android 12+ forbids
                // re-raising a dropped FGS from the background —
                // collectLatest cancels this delay when a newer state
                // lands inside the grace.
                delay(CallForegroundService.STOP_GRACE_MILLIS)
                stopCallService()
            }
        }
    }

    private suspend fun onMediaConnection(connection: CallMediaConnection) {
        if (connection !is CallMediaConnection.Failed) return
        val sid = connectedSid ?: return
        val current = sessionManager.callStore.state.value
        if (current is CallState.Active && current.sid == sid) {
            // Deliberate divergence from the web, which keeps the slot
            // up on mid-call transport death (use-call-engine.ts
            // records telemetry only): on Android an Active slot pins a
            // foreground service and mic capture, so a dead Room must
            // end the call. XEP-0166 §7.4: connectivity problems →
            // <connectivity-error/>. Scoped to the sid this media
            // session belonged to — by the time we run, the slot may
            // already be a migration ring or a fresh call.
            sessionManager.callStore.hangUpActiveIf(sid, WaddleJingleReason.CONNECTIVITY_ERROR)
        }
    }

    private suspend fun disconnectMediaIfConnected() {
        if (connectedSid != null) {
            connectedSid = null
            media.disconnect()
        }
    }

    private suspend fun connectMedia(state: CallState.Active) {
        connectedSid = state.sid
        // XEP-0215: the TURN/STUN advertisement travels over the same
        // XMPP session as the call signaling; a failed fetch degrades
        // to LiveKit's signalling-provided servers.
        val services = sessionManager.callStore.fetchExternalServices().orEmpty()
        try {
            media.connect(state.join, state.media, iceServerConfigsFrom(services))
        } catch (_: CallMediaException) {
            // The Jingle session exists but media can never flow —
            // tear the call down so the peer isn't left on a dead
            // session. XEP-0166 §7.4 <gone/>, matching the web's
            // join-failure teardown reason exactly (CallOverlay.vue
            // tearDownActiveCall(sender, "gone")); the defect itself is
            // visible via media.connection == Failed. Disconnect
            // defensively: a half-open Room must never survive its
            // call slot.
            disconnectMediaIfConnected()
            // Launched on the controller scope, NOT run inline: the
            // teardown flips the slot to Idle, and that emission
            // cancels the collectLatest block this catch runs in — an
            // inline hang-up would cancel its own terminate/finish
            // sends mid-IQ and ghost the peer. Sid-scoped so a peer
            // terminate or fresh call landing before the launched
            // coroutine runs is left untouched.
            scope.launch { sessionManager.callStore.hangUpActiveIf(state.sid, WaddleJingleReason.GONE) }
        }
    }

    private fun startCallService() {
        try {
            context.startForegroundService(CallForegroundService.intent(context))
        } catch (_: ForegroundServiceStartNotAllowedException) {
            // Outgoing/accept flows run foregrounded (UI tap or
            // notification action); the background-reachable
            // migration-takeover transition is covered by the FGS stop
            // grace, which keeps the already-running service alive
            // instead of re-raising it. Residual throws are OEM policy
            // edge cases; the call still works, just without the
            // priority bump.
        }
    }

    private fun stopCallService() {
        context.stopService(CallForegroundService.intent(context))
    }
}

package social.waddle.android.feature.call

import android.app.ForegroundServiceStartNotAllowedException
import android.content.Context
import kotlinx.coroutines.CoroutineScope
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
            CallState.Idle, is CallState.Ended -> {
                disconnectMediaIfConnected()
                stopCallService()
            }
        }
    }

    private suspend fun onMediaConnection(connection: CallMediaConnection) {
        if (connection !is CallMediaConnection.Failed) return
        val sid = connectedSid ?: return
        val current = sessionManager.callStore.state.value
        if (current is CallState.Active && current.sid == sid) {
            // <gone/> like the web's identical media-failure path
            // (CallOverlay.vue tearDownActiveCall(sender, "gone")).
            sessionManager.callStore.hangUp(WaddleJingleReason.GONE)
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
            // media-failure teardown reason exactly (CallOverlay.vue);
            // the defect itself is visible via media.connection ==
            // Failed. Disconnect defensively: a half-open Room must
            // never survive its call slot.
            disconnectMediaIfConnected()
            if (sessionManager.callStore.state.value == state) {
                sessionManager.callStore.hangUp(WaddleJingleReason.GONE)
            }
        }
    }

    private fun startCallService() {
        try {
            context.startForegroundService(CallForegroundService.intent(context))
        } catch (_: ForegroundServiceStartNotAllowedException) {
            // Outgoing/accept flows run foregrounded (UI tap or
            // notification action), so this only fires on OEM policy
            // edge cases; the call still works, just without the
            // priority bump.
        }
    }

    private fun stopCallService() {
        context.stopService(CallForegroundService.intent(context))
    }
}

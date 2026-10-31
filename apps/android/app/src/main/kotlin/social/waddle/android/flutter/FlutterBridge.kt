package social.waddle.android.flutter

import android.content.Intent
import android.net.Uri
import android.view.DragEvent
import androidx.browser.customtabs.CustomTabsIntent
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.ViewModelStore
import io.flutter.embedding.android.FlutterFragmentActivity
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.prefs.TimelineDirection
import social.waddle.android.feature.dm.DmListItem
import social.waddle.android.feature.dm.DmListRow
import social.waddle.android.feature.dm.DmListViewModel
import social.waddle.android.feature.home.HomeUiState
import social.waddle.android.feature.home.HomeViewModel
import social.waddle.android.feature.login.LoginUiState
import social.waddle.android.feature.login.LoginViewModel
import social.waddle.android.feature.rooms.RoomActivity

/** UI-only adapter. AppGraph remains the sole owner of the session and services. */
class FlutterBridge(
    private val activity: FlutterFragmentActivity,
    private val graph: AppGraph,
    messenger: BinaryMessenger,
) : MethodChannel.MethodCallHandler, EventChannel.StreamHandler {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val methods = MethodChannel(messenger, "social.waddle.android/commands")
    private val events = EventChannel(messenger, "social.waddle.android/events")
    private val models = ViewModelStore()
    private var sink: EventChannel.EventSink? = null
    private val snapshots = linkedMapOf<String, Any?>()
    private val login =
        ViewModelProvider(models, LoginViewModel.factory(graph))[LoginViewModel::class.java]
    private val home =
        ViewModelProvider(models, HomeViewModel.factory(graph))[HomeViewModel::class.java]
    private val dms =
        ViewModelProvider(models, DmListViewModel.factory(graph))[DmListViewModel::class.java]
    private val conversations = FlutterConversationCommands(activity, graph, scope, ::emit)
    private val surfaces = FlutterSurfaceCommands(activity, graph, scope)
    var pendingNavigationJid: String? = null
        private set

    private var generation = 0L
    private var observedOwner: String? = null
    private var observedSignedOut = false
    private val communityOwner = MutableStateFlow(false)
    private var hostVisible = false
    private var navigationDelivered = false

    init {
        methods.setMethodCallHandler(this)
        events.setStreamHandler(this)
        observe()
        surfaces.observe(::emit)
        emit("palette", flutterPalette(activity))
    }

    private fun emit(topic: String, data: Any?) {
        // Navigation is a one-shot event. Other topics replay their latest snapshot.
        if (topic != "navigate") snapshots[topic] = data
        sink?.success(mapOf("topic" to topic, "data" to data, "generation" to generation))
    }

    override fun onListen(arguments: Any?, events: EventChannel.EventSink) {
        sink = events
        navigationDelivered = false
        snapshots.toMap().forEach { (topic, data) -> emit(topic, data) }
        conversations.setVisible(hostVisible)
        flushNavigation()
    }

    override fun onCancel(arguments: Any?) {
        sink = null
        conversations.setVisible(false)
    }

    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        val owner = graph.currentSession.value?.sessionId
        val requestGeneration = generation
        scope.launch {
            try {
                val args =
                    (call.arguments as? Map<*, *>)?.entries?.associate {
                        require(it.key is String) { "Argument keys must be strings" }
                        it.key.toString() to it.value
                    } ?: emptyMap()
                val value =
                    when {
                        call.method.startsWith("conversation.") -> {
                            check(graph.appState.value == WaddleAppState.Ready) { "Sign in first" }
                            conversations.execute(call.method, args)
                        }
                        surfaces.supports(call.method) -> {
                            check(graph.appState.value == WaddleAppState.Ready) { "Sign in first" }
                            surfaces.execute(call.method, args)
                        }
                        else -> execute(call.method, args)
                    }
                val sessionChanged =
                    requestGeneration != generation ||
                        owner != graph.currentSession.value?.sessionId
                if (sessionChanged && call.method != "session.logout") {
                    result.error("session_changed", "The account changed. Please try again.", null)
                } else {
                    result.success(value)
                }
            } catch (cancelled: CancellationException) {
                result.error("cancelled", "The operation was cancelled.", null)
                throw cancelled
            } catch (failure: IllegalArgumentException) {
                result.error("invalid_argument", failure.message, null)
            } catch (_: Exception) {
                // Exceptions from auth/network code may carry sensitive URLs. Never expose them.
                result.error(
                    "operation_failed",
                    "The operation could not be completed. Please try again.",
                    null,
                )
            }
        }
    }

    private suspend fun execute(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "ready" -> {
                flushNavigation()
                null
            }
            "navigation.consume" -> {
                if (args["jid"] == pendingNavigationJid) {
                    pendingNavigationJid = null
                    navigationDelivered = false
                }
                null
            }
            "login.providers" -> {
                login.loadProviders()
                null
            }
            "login.select" -> {
                login.selectProvider(args.requiredString("id"))
                null
            }
            "login.cancel" -> {
                login.cancel()
                null
            }
            "session.retry" -> {
                graph.bootstrap.restore()
                graph.sessionManager.requestReconnect()
                null
            }
            "session.logout" -> {
                conversations.close()
                graph.signOut()
                null
            }
            "preferences.set" -> {
                setPreference(args.requiredString("key"), args["value"])
                null
            }
            "link.open" -> {
                val uri = Uri.parse(args.requiredString("url"))
                require(uri.scheme == "https" || uri.scheme == "http") { "An HTTP URL is required" }
                activity.startActivity(Intent(Intent.ACTION_VIEW, uri))
                null
            }
            else -> throw IllegalArgumentException("Unknown command: $method")
        }

    private suspend fun setPreference(key: String, value: Any?) {
        val prefs = graph.userPrefs
        when (key) {
            "theme" -> prefs.setTheme(ThemeMode.fromStorage(value as? String))
            "timeline" ->
                prefs.setTimelineDirection(TimelineDirection.fromStorage(value as? String))
            "wallpaperColors" -> prefs.setWallpaperColors(value as? Boolean ?: false)
            "notificationsEnabled" -> prefs.setNotificationsEnabled(value as? Boolean ?: true)
            "messageSoundsEnabled" -> prefs.setMessageSoundsEnabled(value as? Boolean ?: true)
            "readReceiptsEnabled" -> prefs.setReadReceiptsEnabled(value as? Boolean ?: true)
            "stayConnected" -> prefs.setStayConnected(value as? Boolean)
            else -> throw IllegalArgumentException("Unknown preference")
        }
    }

    private fun observe() {
        observeSession()
        observeHome()
        observeLogin()
        observePreferences()
        scope.launch {
            graph.sessionManager.connectionState.collect {
                emit(
                    "connection",
                    mapOf("state" to it.javaClass.simpleName.replaceFirstChar(Char::lowercase)),
                )
            }
        }
    }

    private fun observeSession() {
        scope.launch {
            combine(graph.appState, graph.currentSession) { state, session ->
                    mapOf(
                        "state" to
                            when (state) {
                                WaddleAppState.Loading -> "loading"
                                WaddleAppState.SignedOut -> "signedOut"
                                WaddleAppState.Ready -> "ready"
                                is WaddleAppState.Error -> "error"
                            },
                        "message" to (state as? WaddleAppState.Error)?.message,
                        "jid" to session?.jid,
                        "username" to session?.username,
                    )
                }
                .collect(::sessionChanged)
        }
    }

    private fun sessionChanged(data: Map<String, String?>) {
        val owner = graph.currentSession.value?.sessionId
        val signedOut = data["state"] == "signedOut"
        if (owner != observedOwner || signedOut != observedSignedOut) {
            generation += 1
            observedOwner = owner
            observedSignedOut = signedOut
            snapshots.keys.filter { it !in ACCOUNT_INDEPENDENT_TOPICS }.forEach(snapshots::remove)
            communityOwner.value = false
            conversations.close()
        }
        if (signedOut) {
            conversations.close()
            snapshots.keys.filter { it !in ACCOUNT_INDEPENDENT_TOPICS }.forEach(snapshots::remove)
            emit(
                "home",
                mapOf(
                    "rooms" to emptyList<Any>(),
                    "people" to emptyList<Any>(),
                    "dmPeers" to emptyList<Any>(),
                ),
            )
        }
        emit("session", data)
        snapshots
            .toMap()
            .filterKeys { it in ACCOUNT_INDEPENDENT_TOPICS }
            .forEach { (topic, snapshot) -> emit(topic, snapshot) }
        if (data["state"] == "ready") {
            flushNavigation()
            probeCommunityOwner()
        }
    }

    private fun probeCommunityOwner() {
        val probeGeneration = generation
        scope.launch {
            val canManage =
                runCatching { graph.sessionManager.isCommunityOwner() }.getOrDefault(false)
            if (generation == probeGeneration) communityOwner.value = canManage
        }
    }

    private fun observeHome() {
        scope.launch {
            combine(home.uiState, dms.items, communityOwner, ::homeSnapshot).collect {
                emit("home", it)
            }
        }
    }

    private fun homeSnapshot(
        state: HomeUiState,
        items: List<DmListItem>,
        canCreate: Boolean,
    ): Map<String, Any?> =
        mapOf(
            "settled" to state.settled,
            "canCreate" to canCreate,
            "isCommunityOwner" to canCreate,
            "rooms" to state.rooms.map(::roomSnapshot) + items.mapNotNull(::groupSnapshot),
            "people" to
                state.people.map {
                    mapOf(
                        "jid" to it.bareJid,
                        "name" to it.displayName,
                        "show" to it.show.name.lowercase(),
                        "inHuddle" to it.inHuddle,
                        "huddleRoomJid" to it.huddleRoomJid,
                    )
                },
            "dmPeers" to items.mapNotNull(::peerSnapshot),
        )

    private fun roomSnapshot(room: RoomActivity): Map<String, Any?> =
        mapOf(
            "jid" to room.roomJid,
            "name" to room.name,
            "description" to room.description,
            "groupDm" to false,
            "forum" to room.forum,
            "unread" to room.unread,
            "mentions" to room.mentions,
            "threadUnread" to room.threadUnread,
            "preview" to room.preview,
            "lastUpdated" to room.lastUpdated,
            "here" to room.here,
            "live" to room.live,
            "video" to room.video,
            "muted" to room.muted,
            "huddle" to room.huddle.map { mapOf("name" to it.nick, "jid" to it.jid) },
        )

    private fun groupSnapshot(item: DmListItem): Map<String, Any?>? {
        val row = item.row as? DmListRow.Group ?: return null
        return mapOf(
            "jid" to row.roomJid,
            "name" to row.name,
            "groupDm" to true,
            "unread" to row.unreadCount,
            "preview" to row.preview,
            "lastUpdated" to row.lastUpdated,
            "muted" to row.isMuted,
            "live" to (item.live.name == "LIVE"),
        )
    }

    private fun peerSnapshot(item: DmListItem): Map<String, Any?>? {
        val row = item.row as? DmListRow.Peer ?: return null
        return mapOf(
            "jid" to row.peerJid,
            "name" to row.name,
            "unread" to row.unreadCount,
            "preview" to row.preview,
            "lastUpdated" to row.lastUpdated,
            "muted" to row.isMuted,
            "show" to item.presence?.name?.lowercase(),
            "live" to item.live.name.lowercase(),
        )
    }

    private fun observeLogin() {
        scope.launch { login.uiState.collect { emit("login", loginSnapshot(it)) } }
        scope.launch {
            login.openVerificationUrl.collect { url ->
                val uri = Uri.parse(url)
                if (uri.scheme == "https" || uri.scheme == "http") {
                    CustomTabsIntent.Builder().build().launchUrl(activity, uri)
                }
            }
        }
    }

    private fun loginSnapshot(state: LoginUiState): Map<String, Any?> =
        when (state) {
            LoginUiState.LoadingProviders -> mapOf("state" to "loading")
            is LoginUiState.ProviderPicker ->
                mapOf(
                    "state" to "providers",
                    "providers" to
                        state.providers.map {
                            mapOf("id" to it.id, "name" to (it.displayName ?: it.id))
                        },
                )
            is LoginUiState.Verifying -> mapOf("state" to "verifying", "code" to state.userCode)
            LoginUiState.Completing -> mapOf("state" to "completing")
            is LoginUiState.Failed ->
                mapOf(
                    "state" to "failed",
                    "error" to "Sign-in could not be completed. Please try again.",
                )
        }

    private fun observePreferences() {
        val prefs = graph.userPrefs
        listOf(
                prefs.theme,
                prefs.timelineDirection,
                prefs.wallpaperColors,
                prefs.notificationsEnabled,
                prefs.messageSoundsEnabled,
                prefs.readReceiptsEnabled,
                prefs.stayConnected,
                graph.stayConnected,
                graph.pushStatus,
                graph.pushDistributor.distributorLabel,
            )
            .forEach { flow -> scope.launch { flow.collect { emitPreferences() } } }
    }

    private suspend fun emitPreferences() {
        val p = graph.userPrefs
        emit(
            "preferences",
            mapOf(
                "theme" to p.theme.first().storageValue,
                "timeline" to p.timelineDirection.first().storageValue,
                "wallpaperColors" to p.wallpaperColors.first(),
                "notificationsEnabled" to p.notificationsEnabled.first(),
                "messageSoundsEnabled" to p.messageSoundsEnabled.first(),
                "readReceiptsEnabled" to p.readReceiptsEnabled.first(),
                "stayConnected" to p.stayConnected.first(),
                "stayConnectedSupported" to graph.stayConnectedSupported,
                "effectiveStayConnected" to graph.stayConnected.value,
                "fcmAvailable" to graph.push.fcmAvailable,
                "distributorLabel" to graph.pushDistributor.distributorLabel.value,
                "pushStatus" to graph.pushStatus.value?.name?.lowercase(),
            ),
        )
    }

    fun navigate(jid: String) {
        pendingNavigationJid = jid
        navigationDelivered = false
        flushNavigation()
    }

    private fun flushNavigation() {
        val jid = pendingNavigationJid ?: return
        if (sink == null || navigationDelivered || graph.appState.value != WaddleAppState.Ready) {
            return
        }
        navigationDelivered = true
        val room =
            graph.sessionManager.roomStore.topology.value.channels.firstOrNull { it.roomJid == jid }
        val isRoom = room != null || jid in graph.sessionManager.roomStore.joinedRooms.value
        emit(
            "navigate",
            mapOf(
                "jid" to jid,
                "group" to isRoom,
                "groupDm" to (room?.isGroupDm == true),
                "title" to (room?.name ?: jid.substringBefore('@')),
            ),
        )
    }

    fun answerPendingCall() {
        val sid = (graph.sessionManager.callStore.state.value as? CallState.Incoming)?.sid ?: return
        scope.launch {
            try {
                surfaces.acceptPendingCall(sid)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                emit(
                    "error",
                    mapOf("message" to "The call could not be answered. Please try again."),
                )
            }
        }
    }

    fun setVisible(visible: Boolean) {
        hostVisible = visible
        conversations.setVisible(visible)
        if (visible) emit("palette", flutterPalette(activity))
    }

    fun handleDrop(event: DragEvent): Boolean = conversations.handleDrop(event)

    fun close() {
        methods.setMethodCallHandler(null)
        events.setStreamHandler(null)
        sink = null
        conversations.dispose()
        surfaces.close()
        models.clear()
        scope.cancel()
        snapshots.clear()
    }
}

private val ACCOUNT_INDEPENDENT_TOPICS = setOf("preferences", "connection", "login", "palette")

internal fun Map<String, Any?>.requiredString(key: String): String =
    (this[key] as? String)?.takeIf { it.isNotBlank() }
        ?: throw IllegalArgumentException("$key is required")

package social.waddle.android.flutter

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import io.flutter.embedding.android.FlutterFragmentActivity
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.ExtensionCommandAction
import social.waddle.android.client.ExtensionCommandCall
import social.waddle.android.client.ExtensionCommandSubmission
import social.waddle.android.client.ExtensionSubmitField
import social.waddle.android.client.GifSearchResult
import social.waddle.android.client.StickerPacksResult
import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.calls.sidOrNull
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.feature.call.CallMediaConnection
import social.waddle.android.feature.conversation.CreateStickerPackResult
import social.waddle.android.feature.conversation.StickerImageInput
import social.waddle.android.feature.profile.ActivityDraft
import social.waddle.android.feature.profile.BitmapAvatarProcessor
import social.waddle.android.feature.profile.MoodDraft
import social.waddle.android.feature.profile.TuneDraft
import social.waddle.android.feature.profile.buildActivityPublication
import social.waddle.android.feature.profile.buildMoodPublication
import social.waddle.android.feature.profile.buildTunePublication
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleVCard4

/** UI-only adapters. AppGraph remains the sole owner of protocol and media lifetimes. */
class FlutterSurfaceCommands(
    private val activity: FlutterFragmentActivity,
    private val graph: AppGraph,
    private val scope: CoroutineScope,
) {
    private val rooms = FlutterSurfaceRooms(graph)
    private val roomCallState = FlutterSurfaceRoomCallState(graph)
    private val projection = FlutterSurfaceProjection(graph)
    private val manager = graph.sessionManager
    private val media = graph.callSessionController.media
    private val commandFields = mutableMapOf<Triple<String, String, String?>, Set<String>>()
    private var permissionWaiter: CompletableDeferred<Map<String, Boolean>>? = null
    private var avatarWaiter: CompletableDeferred<Uri?>? = null
    private var stickersWaiter: CompletableDeferred<List<Uri>>? = null
    private val permissionLauncher =
        activity.registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
            permissionWaiter?.complete(it)
            permissionWaiter = null
        }
    private val avatarLauncher =
        activity.registerForActivityResult(ActivityResultContracts.GetContent()) {
            avatarWaiter?.complete(it)
            avatarWaiter = null
        }
    private val stickersLauncher =
        activity.registerForActivityResult(ActivityResultContracts.GetMultipleContents()) {
            stickersWaiter?.complete(it)
            stickersWaiter = null
        }

    fun supports(method: String): Boolean =
        method.substringBefore('.') in
            setOf(
                "room",
                "group",
                "profile",
                "call",
                "push",
                "directory",
                "admin",
                "gif",
                "stickers",
                "slash",
            )

    suspend fun execute(method: String, args: Map<String, Any?>): Any? =
        when (method.substringBefore('.')) {
            "directory" -> rooms.execute(method, args)
            "admin" -> rooms.execute(method, args)
            "room" -> rooms.execute(method, args)
            "group" -> rooms.execute(method, args)
            "profile" -> profileCommand(method, args)
            "call" -> if (method == "call.roomState") {
                roomCallState.snapshot(args.required("jid"))
            } else {
                callCommand(method, args)
            }
            "push" -> pushCommand(method)
            "gif" -> gifCommand(method, args)
            "stickers" -> stickersCommand(method, args)
            "slash" -> slashCommand(method, args)
            else -> error("Unknown surface command")
        }

    private suspend fun profileCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "profile.load" -> loadProfile(args)
            "profile.save" ->
                checked(
                    manager.publishProfile(
                        WaddleVCard4(
                            fullName = args.optional("fullName")?.take(160),
                            nickname = args.optional("nickname")?.take(80),
                            pronouns = args.optional("pronouns")?.take(48),
                            note = args.optional("note")?.take(500),
                            url = args.optional("url")?.take(240),
                            photoUri = manager.profileStore.selfVcard.value?.photoUri,
                        )
                    )
                )
            "profile.avatarBytes" -> manager.fetchAvatar(normalizedBareJid(args.required("jid")))?.data
            "profile.avatar" -> publishAvatar(args["remove"] == true)
            "profile.mood" ->
                if (args["clear"] == true) {
                    checked(manager.clearMood())
                } else {
                    val value =
                        buildMoodPublication(MoodDraft(args.text("kind"), args.text("text")))
                            ?: error("Choose a mood")
                    checked(manager.setMood(value.kind, value.text))
                }
            "profile.activity" ->
                if (args["clear"] == true) {
                    checked(manager.clearActivity())
                } else {
                    val value =
                        buildActivityPublication(
                                ActivityDraft(
                                    args.text("general"),
                                    args.text("specific"),
                                    args.text("text"),
                                )
                            )
                            .publication
                            ?: error("Choose an activity and a valid specific activity")
                    checked(manager.setActivity(value.general, value.specific, value.text))
                }
            "profile.tune" -> publishTune(args)
            else -> error("Unknown profile command")
        }

    private suspend fun loadProfile(args: Map<String, Any?>): Map<String, Any?> {
        val requested = args.optional("jid")?.let(::normalizedBareJid)
        return if (requested != null && requested != graph.currentSession.value?.jid?.let(::normalizedBareJid)) {
            projection.peer(requested)
        } else {
            checked(manager.loadSelfProfile())
            projection.profile()
        }
    }

    private suspend fun callCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "call.start" -> startCall(args)
            "call.accept" -> acceptPendingCall(incomingSid())
            "call.decline" -> {
                check(
                    graph.applicationScope.async { manager.callStore.declineIncoming() }.await()
                ) {
                    "Call is no longer ringing"
                }
                null
            }
            "call.hangup" -> {
                graph.applicationScope.async { manager.callStore.hangUp() }.await()
                null
            }
            "call.leaveRetained" -> leaveRetained(args.required("jid"))
            "call.dismiss" -> {
                manager.callStore.dismiss()
                null
            }
            "call.microphone" -> setMicrophone(args)
            "call.camera" -> setCamera(args)
            "call.speaker" -> setSpeaker(args)
            "call.flipCamera" -> {
                media.flipCamera()
                null
            }
            "call.hand" -> {
                check(
                    manager.callStore.muc.setHandRaised(
                        args["raised"] as? Boolean ?: !manager.callStore.muc.selfHandRaised.value
                    )
                )
                null
            }
            else -> error("Unknown call command")
        }

    private suspend fun leaveRetained(jid: String): Any? {
        val nick = ownNick()
        val fullJid = manager.ownFullJid()
        check(
            graph.applicationScope
                .async { manager.callStore.muc.leaveRetained(jid, nick, fullJid) }
                .await()
        ) {
            "Could not leave the retained call"
        }
        return null
    }

    private suspend fun startCall(args: Map<String, Any?>): Any? {
        val video = args["video"] == true
        requestCapture(video)
        val jid = args.required("jid")
        val callMedia = WaddleCallMedia(audio = true, video = video)
        val ok =
            graph.applicationScope
                .async {
                    if (args["group"] == true) {
                        val nick = ownNick()
                        val selfFullJid = manager.ownFullJid()
                        if (manager.callStore.muc.resume(jid, nick, selfFullJid)) {
                            true
                        } else {
                            val domain = graph.currentSession.value?.jid?.substringAfter('@')
                            manager.callStore.muc.begin(
                                jid,
                                callMedia,
                                nick,
                                selfFullJid,
                                expectedMixerJid =
                                    domain?.takeIf { it.isNotBlank() }?.let { "calls.$it" },
                            )
                        }
                    } else {
                        manager.callStore.startCall(jid, callMedia)
                    }
                }
                .await()
        check(ok) { manager.callStore.lastError.value ?: "Could not start call" }
        return null
    }

    private fun incomingSid(): String =
        (manager.callStore.state.value as? CallState.Incoming)?.sid ?: error("Call is no longer ringing")

    suspend fun acceptPendingCall(expectedSid: String): Any? {
        val incoming =
            manager.callStore.state.value as? CallState.Incoming
                ?: error("Call is no longer ringing")
        check(incoming.sid == expectedSid) { "Call is no longer ringing" }
        requestCapture(incoming.media.video)
        check((manager.callStore.state.value as? CallState.Incoming)?.sid == incoming.sid) {
            "Call is no longer ringing"
        }
        check(
            graph.applicationScope.async { manager.callStore.acceptIncoming(expectedSid) }.await()
        ) {
            "Call is no longer available"
        }
        return null
    }

    private suspend fun setMicrophone(args: Map<String, Any?>): Any? {
        val sid = manager.callStore.state.value.sidOrNull
        val enable = args["enabled"] as? Boolean ?: !media.micEnabled.value
        if (enable) requestCapture(false)
        check(manager.callStore.state.value.sidOrNull == sid) {
            "Call changed during permission request"
        }
        media.setMicEnabled(enable)
        if ((manager.callStore.state.value as? CallState.Active)?.kind == CallKind.MUC) {
            check(manager.callStore.muc.broadcastSelfMute(!enable)) {
                "Could not update mute presence"
            }
        }
        return null
    }

    private fun setSpeaker(args: Map<String, Any?>): Any? {
        media.setSpeakerphone(args["enabled"] as? Boolean ?: !media.speakerOn.value)
        return null
    }

    private suspend fun setCamera(args: Map<String, Any?>): Any? {
        val sid = manager.callStore.state.value.sidOrNull
        val enable = args["enabled"] as? Boolean ?: !media.cameraEnabled.value
        if (enable) requestCapture(true)
        check(manager.callStore.state.value.sidOrNull == sid) {
            "Call changed during permission request"
        }
        media.setCameraEnabled(enable)
        return null
    }

    private suspend fun publishTune(args: Map<String, Any?>): Any? =
        if (args["clear"] == true) {
            checked(manager.clearTune())
        } else {
            val value =
                buildTunePublication(
                        TuneDraft(
                            args.text("artist"),
                            args.text("title"),
                            args.text("source"),
                            (args["lengthSeconds"] ?: args["length"])?.toString().orEmpty(),
                            args["rating"]?.toString().orEmpty(),
                            args.text("track"),
                            args.text("uri"),
                        )
                    )
                    .publication ?: error("Enter valid tune details")
            checked(manager.setTune(value))
        }

    private fun pushCommand(method: String): Any? =
        when (method) {
            "push.settings" -> {
                activity.startActivity(
                    Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                        .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
                )
                null
            }
            "push.choose" -> {
                graph.push.useUnifiedPush(activity)
                null
            }
            "push.fcm" -> {
                check(!graph.stayConnectedSupported) { "FCM is unavailable in this distribution" }
                graph.push.useFcm()
                null
            }
            else -> error("Unknown push command")
        }

    private suspend fun gifCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "gif.search" ->
                when (val result = graph.gifSearchGateway.search(args.text("query"))) {
                    is GifSearchResult.Results ->
                        result.gifs.map {
                            mapOf(
                                "id" to it.id,
                                "title" to it.title,
                                "previewUrl" to it.previewUrl,
                                "originalUrl" to it.originalUrl,
                            )
                        }
                    GifSearchResult.NotConfigured ->
                        error("GIF search is not configured on this server")
                    GifSearchResult.RateLimited ->
                        error("GIF search rate limit reached. Try again shortly")
                    GifSearchResult.Unavailable -> error("GIF search unavailable")
                }
            else -> error("Unknown gif command")
        }

    private suspend fun stickersCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "stickers.load" -> {
                manager.loadStickerPacks()
                projection.stickers()
            }
            "stickers.remove" -> checked(manager.removeStickerPack(args.required("packId")))
            "stickers.create" -> createStickers(args)
            else -> error("Unknown stickers command")
        }

    private suspend fun slashCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "slash.list" ->
                manager.discoverExtensionCommands().map {
                    mapOf(
                        "serviceJid" to it.serviceJid,
                        "node" to it.node,
                        "name" to it.name,
                        "scope" to it.scope.name.lowercase(),
                        "composerPrefix" to it.composerPrefix,
                        "inlineField" to it.inlineField,
                        "composerExecute" to it.composerExecute,
                    )
                }
            "slash.invoke" ->
                commandResult(
                    manager.invokeExtensionCommand(
                        args.required("serviceJid"),
                        args.required("node"),
                        args.optional("jid"),
                    ),
                    args.required("serviceJid"),
                    args.required("node"),
                )
            "slash.submit" -> submitSlash(args)
            else -> error("Unknown slash command")
        }

    private suspend fun submitSlash(args: Map<String, Any?>): Any? {
        val fields =
            (args["fields"] as? List<*>).orEmpty().map { field ->
                val map = field as? Map<*, *> ?: error("Invalid form field")
                ExtensionSubmitField(
                    map["name"] as? String ?: error("Missing field name"),
                    (map["values"] as? List<*>)
                        ?.map { it as? String ?: error("Invalid field value") }
                        .orEmpty(),
                )
            }
        val allowed =
            commandFields[
                Triple(
                    args.required("serviceJid"),
                    args.required("node"),
                    args.optional("sessionId"),
                )
            ] ?: error("This command form is no longer active")
        check(fields.all { it.name in allowed }) {
            "A blocked or unknown field cannot be submitted"
        }
        return commandResult(
            manager.submitExtensionCommandForm(
                ExtensionCommandSubmission(
                    args.required("serviceJid"),
                    args.required("node"),
                    args.optional("sessionId"),
                    fields,
                    ExtensionCommandAction.valueOf(
                        (args.optional("action") ?: "complete").uppercase()
                    ),
                    args.optional("jid"),
                )
            ),
            args.required("serviceJid"),
            args.required("node"),
        )
    }

    fun observe(emit: (String, Any?) -> Unit) {
        val store = manager.profileStore
        scope.launch { graph.currentSession.collect { commandFields.clear() } }
        scope.launch {
            combine(store.selfVcard, store.selfMood, store.selfActivity, store.selfTune) {
                    _,
                    _,
                    _,
                    _ ->
                    projection.profile()
                }
                .collect { emit("profile", it) }
        }
        scope.launch { store.avatars.collect { emit("profile", projection.profile()) } }
        observeCall(emit)
        scope.launch {
            manager.stickerPackStore.packs.collect {
                emit(
                    "stickers",
                    mapOf(
                        "packs" to
                            if (it is StickerPacksResult.Ready || it == StickerPacksResult.Empty) {
                                projection.stickers()
                            } else {
                                emptyList<Any>()
                            },
                        "state" to
                            if (it == StickerPacksResult.Unavailable) {
                                "unavailable"
                            } else if (it == null) "loading" else "ready",
                    ),
                )
            }
        }
    }

    private fun observeCall(emit: (String, Any?) -> Unit) {
        scope.launch {
            combine(
                    combine(manager.callStore.state, manager.callStore.muc.selfHandRaised, ::Pair),
                    media.micEnabled,
                    media.cameraEnabled,
                    media.speakerOn,
                    media.remoteParticipantIdentities,
                ) { state, mic, camera, speaker, participants ->
                    projection.call(state.first) +
                        mapOf(
                            "handRaised" to state.second,
                            "microphoneEnabled" to mic,
                            "cameraEnabled" to camera,
                            "speakerEnabled" to speaker,
                            "participants" to
                                participants.map {
                                    mapOf("jid" to it, "name" to it.substringBefore('@'))
                                },
                        )
                }
                .collect { emit("call", it) }
        }
        scope.launch {
            media.connection.collect {
                emit(
                    "callMedia",
                    mapOf(
                        "state" to
                            when (it) {
                                CallMediaConnection.Idle -> "idle"
                                CallMediaConnection.Connecting -> "connecting"
                                CallMediaConnection.Connected -> "connected"
                                CallMediaConnection.Reconnecting -> "reconnecting"
                                is CallMediaConnection.Failed -> "failed"
                            },
                        "error" to (it as? CallMediaConnection.Failed)?.message,
                    ),
                )
            }
        }
    }

    fun close() {
        permissionWaiter?.cancel()
        avatarWaiter?.cancel()
        stickersWaiter?.cancel()
        permissionLauncher.unregister()
        avatarLauncher.unregister()
        stickersLauncher.unregister()
    }

    private suspend fun requestCapture(video: Boolean) {
        val owner = graph.currentSession.value ?: error("Sign in first")
        val permissions =
            buildList {
                    add(Manifest.permission.RECORD_AUDIO)
                    if (video) add(Manifest.permission.CAMERA)
                }
                .filter {
                    ContextCompat.checkSelfPermission(activity, it) !=
                        PackageManager.PERMISSION_GRANTED
                }
        if (permissions.isEmpty()) return
        check(permissionWaiter == null) { "A permission request is already open" }
        val waiter = CompletableDeferred<Map<String, Boolean>>()
        permissionWaiter = waiter
        permissionLauncher.launch(permissions.toTypedArray())
        waiter.await()
        check(graph.currentSession.value === owner) { "Account changed during permission request" }
        // The native media controller publishes only granted tracks. Denial keeps the
        // call receive-only, matching the existing Android calling behavior.
    }

    private suspend fun publishAvatar(remove: Boolean): Any? {
        if (remove) return checked(manager.disableAvatar())
        check(avatarWaiter == null) { "An image picker is already open" }
        val owner = graph.currentSession.value ?: error("Sign in first")
        val waiter = CompletableDeferred<Uri?>()
        avatarWaiter = waiter
        avatarLauncher.launch("image/*")
        val uri = waiter.await() ?: return null
        val processed =
            BitmapAvatarProcessor(activity.contentResolver).process(uri)
                ?: error("Could not read image")
        check(graph.currentSession.value === owner) { "Account changed while picking image" }
        return checked(
            manager.publishAvatar(
                processed.data,
                processed.mimeType,
                processed.width.toUInt(),
                processed.height.toUInt(),
            )
        )
    }

    private suspend fun createStickers(args: Map<String, Any?>): Any? {
        check(stickersWaiter == null) { "An image picker is already open" }
        val owner = graph.currentSession.value ?: error("Sign in first")
        val waiter = CompletableDeferred<List<Uri>>()
        stickersWaiter = waiter
        stickersLauncher.launch("image/*")
        val images = waiter.await()
        if (images.isEmpty()) return null
        check(graph.currentSession.value === owner) { "Account changed while picking images" }
        val name = args.required("name")
        val result =
            graph.stickerPackCreator.create(
                name,
                args.optional("summary"),
                images.map { StickerImageInput(it, name) },
            )
        check(result == CreateStickerPackResult.Ok) { "Could not create sticker pack" }
        return projection.stickers()
    }

    private fun commandResult(
        call: ExtensionCommandCall,
        serviceJid: String,
        node: String,
    ): Map<String, Any?> =
        when (call) {
            is ExtensionCommandCall.Failed -> error(call.detail ?: "Command failed")
            is ExtensionCommandCall.Ok -> {
                val result = call.result
                commandFields[Triple(serviceJid, node, result.sessionId)] =
                    result.form
                        ?.fields
                        ?.filterNot { it.blocked }
                        ?.map { it.name }
                        ?.toSet()
                        .orEmpty()
                projection.command(result)
            }
        }

    private fun ownNick(): String =
        graph.currentSession.value?.xmppLocalpart ?: error("Sign in first")
}

internal fun Map<String, Any?>.text(key: String): String = this[key] as? String ?: ""

internal fun Map<String, Any?>.optional(key: String): String? = text(key).trim().ifEmpty { null }

internal fun Map<String, Any?>.required(key: String): String =
    optional(key) ?: error("Missing $key")

internal fun Map<String, Any?>.strings(key: String): List<String> =
    (this[key] as? List<*>)?.map { it as? String ?: error("Invalid $key") } ?: error("Missing $key")

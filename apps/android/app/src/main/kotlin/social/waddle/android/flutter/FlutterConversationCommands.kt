package social.waddle.android.flutter

import android.content.Intent
import android.view.DragEvent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.net.toUri
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.ViewModelStore
import io.flutter.embedding.android.FlutterFragmentActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.MentionRef
import social.waddle.android.client.StickerPacksResult
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.directoryMentionCandidatesOf
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.feature.channel.ChannelViewModel
import social.waddle.android.feature.conversation.ComposerMode
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.feature.conversation.ThreadViewModel
import social.waddle.android.feature.dm.DmViewModel
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddleSharedFile

/** Reuses the existing conversation engine, including outbox, identity and read-marker rules. */
internal class FlutterConversationCommands(
    private val activity: FlutterFragmentActivity,
    private val graph: AppGraph,
    private val scope: CoroutineScope,
    private val emit: (String, Any?) -> Unit,
) {
    private var store = ViewModelStore()
    private var model: ConversationViewModel? = null
    private var subscription: Job? = null
    private var jid: String? = null
    private var group = false
    private var threadId: String? = null
    private var viewId: Any? = null
    private var hostVisible = false
    private var routeVisible = true
    private var pickedFor: ConversationViewModel? = null
    private var pickedOwner: String? = null
    private val contentInsertion =
        FlutterContentInsertion(activity, scope) {
            model?.takeIf {
                hostVisible && routeVisible && graph.appState.value == WaddleAppState.Ready
            }
        }
    private val picker =
        activity.registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            val target = pickedFor
            pickedFor = null
            val sameOwner = pickedOwner == graph.currentSession.value?.sessionId
            val ready = graph.appState.value == WaddleAppState.Ready
            if (uri != null && target === model && sameOwner) {
                if (ready) target?.sendAttachment(uri)
            }
        }

    suspend fun execute(method: String, args: Map<String, Any?>): Any? {
        if (method == "conversation.open") {
            openConversation(args)
            return null
        }
        if (method == "conversation.close") {
            if (args["jid"] == jid && ownsView(args)) close()
            return null
        }
        require(args.requiredString("jid") == jid) { "Open this conversation first" }
        require(ownsView(args)) { "This conversation view is no longer active" }
        val vm = requireNotNull(model) { "Open this conversation first" }
        return when (method) {
            "conversation.send",
            "conversation.edit",
            "conversation.reply",
            "conversation.cancelReply",
            "conversation.cancelEdit",
            "conversation.retry",
            "conversation.gif",
            "conversation.sticker" -> sendCommand(method, args, vm)
            "conversation.history",
            "conversation.read",
            "conversation.visibility",
            "conversation.typing",
            "conversation.reaction",
            "conversation.retract",
            "conversation.moderate",
            "conversation.pin",
            "conversation.notify" -> actionCommand(method, args, vm)
            "conversation.attach",
            "conversation.insertContent",
            "conversation.pasteImage",
            "conversation.fileBytes",
            "conversation.fileOpen" -> fileCommand(method, args, vm)
            else -> queryCommand(method, args)
        }
    }

    private fun ownsView(args: Map<String, Any?>): Boolean =
        args["viewId"] == null || args["viewId"] == viewId

    private fun item(args: Map<String, Any?>): TimelineItem =
        requireNotNull(
            graph.sessionManager.timelineStore
                .timeline(args.requiredString("jid"))
                .value
                .firstOrNull { it.id == args["targetId"] || it.presentationId == args["targetId"] }
        ) {
            "The message is no longer available"
        }

    private fun sendCommand(
        method: String,
        args: Map<String, Any?>,
        vm: ConversationViewModel,
    ): Any? {
        when (method) {
            "conversation.send" -> send(args, vm)
            "conversation.edit" -> {
                val target = item(args)
                require(target.isMine && target.tombstone == null) {
                    "Only your messages can be edited"
                }
                vm.startEdit(target)
                (args["body"] as? String)?.let { vm.send(it) }
            }
            "conversation.reply" -> vm.startReply(item(args))
            "conversation.cancelReply" -> vm.cancelReply()
            "conversation.cancelEdit" -> vm.cancelEdit()
            "conversation.retry" -> vm.retry((args["localId"] as Number).toLong())
            "conversation.gif" -> vm.sendGif(args.requiredString("url"))
            "conversation.sticker" -> sendSticker(args, vm)
        }
        return null
    }

    private fun send(args: Map<String, Any?>, vm: ConversationViewModel) {
        (args["replyTo"] as? String)?.let { target ->
            graph.sessionManager.timelineStore
                .timeline(args.requiredString("jid"))
                .value
                .firstOrNull { it.id == target }
                ?.let(vm::startReply)
        }
        val mentions =
            (args["mentions"] as? List<*>).orEmpty().map { value ->
                val mention = requireNotNull(value as? Map<*, *>) { "Invalid mention" }
                val uri = requireNotNull(mention["uri"] as? String) { "Mention URI required" }
                val begin =
                    requireNotNull(mention["begin"] as? Number) { "Mention start required" }
                        .toLong()
                val end =
                    requireNotNull(mention["end"] as? Number) { "Mention end required" }.toLong()
                require(begin >= 0 && end >= begin) { "Invalid mention range" }
                MentionRef(uri, begin.toUInt(), end.toUInt())
            }
        vm.send(args["body"] as? String ?: "", mentions)
    }

    private fun sendSticker(args: Map<String, Any?>, vm: ConversationViewModel) {
        val packs = graph.sessionManager.stickerPackStore.packs.value as? StickerPacksResult.Ready
        val pack =
            requireNotNull(packs?.packs?.firstOrNull { it.id == args["packId"] }) {
                "The sticker pack is unavailable"
            }
        val index =
            requireNotNull(args["index"] as? Number) { "A sticker index is required" }.toInt()
        val sticker =
            requireNotNull(pack.stickers.getOrNull(index)) { "The sticker is unavailable" }
        vm.sendSticker(sticker, pack.id)
    }

    private fun actionCommand(
        method: String,
        args: Map<String, Any?>,
        vm: ConversationViewModel,
    ): Any? {
        when (method) {
            "conversation.history" -> vm.loadOlder()
            "conversation.read" ->
                vm.onAtNewestEdgeChanged(args["atNewestEdge"] as? Boolean ?: true)
            "conversation.visibility" -> setRouteVisible(args["visible"] == true)
            "conversation.typing" -> vm.onDraftChanged()
            "conversation.reaction" -> vm.toggleReaction(item(args), args.requiredString("emoji"))
            "conversation.retract" -> vm.retract(item(args))
            "conversation.moderate" -> vm.moderate(item(args))
            "conversation.pin" -> vm.setPinned(item(args), args["pinned"] as? Boolean ?: true)
            "conversation.notify" -> {
                val value = args.requiredString("mode")
                val mode =
                    requireNotNull(
                        WaddleNotifyMode.entries.firstOrNull { it.name.equals(value, true) }
                    ) {
                        "Unknown notification mode"
                    }
                vm.setNotificationMode(mode)
            }
        }
        return null
    }

    private suspend fun fileCommand(
        method: String,
        args: Map<String, Any?>,
        vm: ConversationViewModel,
    ): Any? =
        when (method) {
            "conversation.insertContent" -> insertContent(args, vm)
            "conversation.pasteImage" -> contentInsertion.pasteImage(vm)
            "conversation.attach" -> {
                pickedFor = vm
                pickedOwner = graph.currentSession.value?.sessionId
                picker.launch(arrayOf("*/*"))
                null
            }
            "conversation.fileBytes" -> {
                val file = fileOf(item(args), args)
                val encrypted = requireNotNull(file.encrypted) { "This file is not encrypted" }
                require(file.mediaType?.startsWith("image/") == true) {
                    "Only images can be displayed inline"
                }
                graph.encryptedAttachmentDownloader.fetchAndDecrypt(
                    file.url,
                    encrypted,
                    file.size?.toLong(),
                ) ?: error("Unable to decrypt image")
            }
            else -> {
                val file = fileOf(item(args), args)
                if (file.encrypted != null) {
                    check(graph.encryptedAttachmentOpener.open(file)) { "Unable to open file" }
                } else {
                    val uri = file.url.toUri()
                    require(uri.scheme == "https" || uri.scheme == "http") {
                        "Unsupported file URL"
                    }
                    activity.startActivity(Intent(Intent.ACTION_VIEW, uri))
                }
                null
            }
        }

    private fun insertContent(args: Map<String, Any?>, vm: ConversationViewModel): Boolean {
        val data = args["data"] as? ByteArray
        if (data != null) {
            return contentInsertion.insertBytes(data, args.requiredString("mimeType"), vm)
        }
        return contentInsertion.insert(args.requiredString("uri").toUri(), vm)
    }

    private suspend fun queryCommand(method: String, args: Map<String, Any?>): Any? {
        val requested = args.requiredString("jid")
        return when (method) {
            "conversation.mentionSearch" -> mentionSearch(requested, args["query"] as? String ?: "")
            "conversation.search" -> search(requested, args.requiredString("query"))
            "conversation.members" -> {
                graph.sessionManager.refreshRoomMembers(requested)
                graph.sessionManager.roomMembersStore.rooms.value[requested]
                    ?.members
                    .orEmpty()
                    .map {
                        mapOf(
                            "jid" to it.jid,
                            "name" to (it.nick ?: it.jid.substringBefore('@')),
                            "affiliation" to it.affiliation.name.lowercase(),
                        )
                    }
            }
            else -> throw IllegalArgumentException("Unknown conversation command")
        }
    }

    private suspend fun search(requested: String, query: String): Any? {
        val page =
            if (group) {
                graph.sessionManager.searchRoomHistory(requested, query, 50u)
            } else {
                graph.sessionManager.searchDmHistory(requested, query, 50u)
            }
        check(page != null) { "Search is unavailable while disconnected" }
        check(page.messages.isNotEmpty() || page.isComplete) { "The archive search failed" }
        return page.messages.map { archived ->
            val removed = archived.isRetracted || archived.moderationTargetId != null
            mapOf(
                "id" to (archived.stanzaId ?: archived.originId ?: archived.id ?: archived.mamId),
                "body" to
                    if (removed) {
                        "Message removed"
                    } else {
                        archived.body
                    },
                "author" to archived.from?.substringAfter('/'),
                "timestamp" to archived.timestamp,
                "mine" to false,
            )
        }
    }

    private suspend fun mentionSearch(requested: String, query: String): Any? {
        val occupants =
            model?.mentionCandidates?.value.orEmpty().filter {
                !it.isBroadcast && it.display.contains(query, ignoreCase = true)
            }
        val entries =
            if (query.isBlank()) {
                emptyList()
            } else {
                graph.sessionManager.searchRoomMentionUsers(requested, query).orEmpty()
            }
        val directory = directoryMentionCandidatesOf(entries, graph.currentSession.value?.jid)
        return (occupants + directory)
            .distinctBy { it.uri }
            .map { mapOf("jid" to it.jid, "name" to it.display, "uri" to it.uri) }
    }

    private fun openConversation(args: Map<String, Any?>) {
        val target = args.requiredString("jid")
        val isGroup = args["group"] as? Boolean ?: false
        val thread = args["threadId"] as? String
        val ownerView = args["viewId"]
        val atNewestEdge = args["atNewestEdge"] as? Boolean ?: true
        val sameDestination = target == jid && isGroup == group
        val sameOwner = thread == threadId && ownerView == viewId
        if (model != null && sameDestination && sameOwner) {
            model?.onAtNewestEdgeChanged(atNewestEdge)
            routeVisible = true
            updateVisibility()
            return
        }
        close()
        jid = target
        group = isGroup
        threadId = thread
        viewId = ownerView
        routeVisible = true
        val factory =
            when {
                thread != null -> ThreadViewModel.factory(graph, target, isGroup, thread)
                isGroup -> ChannelViewModel.factory(graph, target)
                else -> DmViewModel.factory(graph, target)
            }
        val modelClass: Class<out ConversationViewModel> =
            when {
                thread != null -> ThreadViewModel::class.java
                isGroup -> ChannelViewModel::class.java
                else -> DmViewModel::class.java
            }
        val vm = ViewModelProvider(store, factory)[modelClass]
        model = vm
        vm.onAtNewestEdgeChanged(atNewestEdge)
        updateVisibility()
        subscribe(vm, target, isGroup, thread, ownerView)
    }

    private fun subscribe(
        vm: ConversationViewModel,
        target: String,
        isGroup: Boolean,
        thread: String?,
        ownerView: Any?,
    ) {
        subscription =
            scope.launch {
                launch { observeState(vm, target, isGroup, thread, ownerView) }
                launch {
                    vm.actionFailures.collect {
                        emit(
                            "error",
                            mapOf("message" to "The message action failed. Please try again."),
                        )
                    }
                }
                launch {
                    vm.notifySettingsMismatch.collect {
                        emit(
                            "error",
                            mapOf(
                                "message" to
                                    "Notification preferences could not be saved for this conversation."
                            ),
                        )
                    }
                }
                launch {
                    vm.composerMode.collect { mode ->
                        emit(
                            "composer",
                            composerMap(target, mode) +
                                mapOf("viewId" to ownerView, "threadId" to thread),
                        )
                    }
                }
            }
    }

    private suspend fun observeState(
        vm: ConversationViewModel,
        target: String,
        isGroup: Boolean,
        thread: String?,
        ownerView: Any?,
    ) {
        val snapshots =
            combine(vm.uiState, vm.typing, vm.notifyMode, vm.uploadState) {
                state,
                typing,
                notify,
                upload ->
                mapOf(
                    "jid" to target,
                    "group" to isGroup,
                    "threadId" to thread,
                    "viewId" to ownerView,
                    "messages" to
                        state.entries.map { entry ->
                            FlutterConversationProjection.row(entry, isGroup)
                        },
                    "typing" to typing,
                    "loading" to state.awaitingFirstPage,
                    "loadingOlder" to state.isLoadingOlder,
                    "reachedStart" to state.reachedHistoryStart,
                    "newestFirst" to state.newestFirst,
                    "canPin" to state.canPin,
                    "canModerate" to state.canModerate,
                    "pinnedIds" to state.pinnedIds.toList(),
                    "notifyMode" to notify.name.lowercase(),
                    "uploadState" to upload.javaClass.simpleName.lowercase(),
                    "threads" to
                        state.threads.map {
                            mapOf(
                                "threadId" to it.threadId,
                                "author" to it.rootAuthor,
                                "body" to
                                    if (it.rootTombstoned) {
                                        "Message removed"
                                    } else {
                                        it.rootPreview
                                    },
                                "replyCount" to it.replyCount,
                                "timestamp" to it.lastTimestamp,
                            )
                        },
                )
            }
        val channel = vm as? ChannelViewModel
        combine(
                snapshots,
                channel?.isRoomOwner ?: flowOf(false),
                channel?.canManageMembers ?: flowOf(false),
            ) { snapshot, owner, members ->
                snapshot + mapOf("canConfigure" to owner, "canManageMembers" to members)
            }
            .collect { emit("conversation", it) }
    }

    private fun composerMap(target: String, mode: ComposerMode): Map<String, Any?> =
        when (mode) {
            ComposerMode.Normal -> mapOf("jid" to target, "mode" to "normal")
            is ComposerMode.Editing ->
                mapOf(
                    "jid" to target,
                    "mode" to "editing",
                    "targetId" to mode.targetId,
                    "body" to mode.originalBody,
                    "attempt" to mode.attempt,
                )
            is ComposerMode.Replying ->
                mapOf(
                    "jid" to target,
                    "mode" to "replying",
                    "targetId" to mode.targetId,
                    "author" to mode.authorName,
                    "body" to mode.previewBody,
                )
        }

    fun setVisible(value: Boolean) {
        hostVisible = value
        updateVisibility()
    }

    private fun setRouteVisible(value: Boolean) {
        routeVisible = value
        updateVisibility()
    }

    private fun updateVisibility() {
        if (hostVisible && routeVisible) {
            model?.onConversationVisible()
        } else {
            model?.onConversationHidden()
        }
    }

    fun close() {
        contentInsertion.close()
        model?.onConversationHidden()
        subscription?.cancel()
        subscription = null
        model = null
        pickedFor = null
        pickedOwner = null
        jid = null
        viewId = null
        store.clear()
        store = ViewModelStore()
    }

    fun dispose() {
        close()
        picker.unregister()
    }

    fun handleDrop(event: DragEvent): Boolean = contentInsertion.handleDrop(event)

    private fun fileOf(item: TimelineItem, args: Map<String, Any?>): WaddleSharedFile {
        require(item.tombstone == null) { "This message was removed" }
        val index =
            (args["fileId"] as? String)?.toIntOrNull() ?: (args["fileId"] as? Number)?.toInt() ?: 0
        return item.sharedFiles.getOrNull(index)
            ?: throw IllegalArgumentException("The attachment is no longer available")
    }
}

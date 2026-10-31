package social.waddle.android.flutter

import social.waddle.android.AppGraph
import social.waddle.android.client.ExtensionCommandResult
import social.waddle.android.client.StickerPacksResult
import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState
import social.waddle.android.client.calls.sidOrNull

/** Explicit public presentation fields; session and media credentials never cross the channel. */
internal class FlutterSurfaceProjection(private val graph: AppGraph) {
    private val manager = graph.sessionManager

    suspend fun peer(jid: String): Map<String, Any?> {
        val person = manager.peopleDirectory.value.people.firstOrNull { it.bareJid == jid }
        val contact = manager.presenceStore.contacts.value[jid]
        val avatar = manager.fetchAvatar(jid)
        return mapOf(
            "jid" to jid,
            "fullName" to (person?.displayName ?: jid.substringBefore('@')),
            "nickname" to person?.displayName,
            "note" to contact?.presence?.status,
            "status" to contact?.presence?.status,
            "presence" to (person?.show ?: contact?.show)?.name?.lowercase(),
            "avatar" to avatar?.data,
            "rooms" to person?.roomJids.orEmpty(),
            "inHuddle" to (person?.inHuddle == true),
        )
    }

    fun profile(): Map<String, Any?> {
        val store = manager.profileStore
        val card = store.selfVcard.value
        val jid = graph.currentSession.value?.jid
        return mapOf(
            "jid" to jid,
            "fullName" to card?.fullName,
            "nickname" to card?.nickname,
            "pronouns" to card?.pronouns,
            "note" to card?.note,
            "url" to card?.url,
            "avatar" to jid?.let { store.avatars.value[it]?.data },
            "mood" to store.selfMood.value?.let { mapOf("kind" to it.kind, "text" to it.text) },
            "activity" to
                store.selfActivity.value?.let {
                    mapOf("general" to it.general, "specific" to it.specific, "text" to it.text)
                },
            "tune" to
                store.selfTune.value?.let {
                    mapOf(
                        "artist" to it.artist,
                        "title" to it.title,
                        "source" to it.source,
                        "lengthSeconds" to it.lengthSeconds?.toLong(),
                        "rating" to it.rating?.toInt(),
                        "track" to it.track,
                        "uri" to it.uri,
                    )
                },
        )
    }

    fun call(state: CallState): Map<String, Any?> =
        when (state) {
            CallState.Idle -> mapOf("state" to "idle")
            is CallState.Incoming ->
                mapOf(
                    "state" to "incoming",
                    "peerJid" to state.from,
                    "video" to state.media.video,
                    "accepting" to state.accepting,
                )
            is CallState.Outgoing ->
                mapOf(
                    "state" to "outgoing",
                    "peerJid" to state.to,
                    "video" to state.media.video,
                    "ringing" to state.ringing,
                )
            is CallState.MucPending ->
                mapOf(
                    "state" to "connecting",
                    "peerJid" to state.roomJid,
                    "video" to state.media.video,
                    "group" to true,
                )
            is CallState.Active ->
                mapOf(
                    "state" to "active",
                    "peerJid" to state.peer,
                    "video" to state.media.video,
                    "group" to (state.kind == CallKind.MUC),
                )
            is CallState.Ended -> mapOf("state" to "ended", "reason" to state.reason?.toString())
        } + ("sid" to state.sidOrNull)

    fun stickers(): List<Map<String, Any?>> =
        when (val result = manager.stickerPackStore.packs.value) {
            is StickerPacksResult.Ready ->
                result.packs.map { pack ->
                    mapOf(
                        "id" to pack.id,
                        "name" to pack.name,
                        "summary" to pack.summary,
                        "stickers" to
                            pack.stickers.mapIndexed { index, sticker ->
                                mapOf(
                                    "index" to index,
                                    "desc" to sticker.desc,
                                    "url" to sticker.sources.firstOrNull(),
                                )
                            },
                    )
                }
            StickerPacksResult.Empty,
            null -> emptyList()
            StickerPacksResult.Unavailable -> error("Sticker packs unavailable while disconnected")
        }

    fun command(result: ExtensionCommandResult): Map<String, Any?> =
        mapOf(
            "status" to result.status.name.lowercase(),
            "sessionId" to result.sessionId,
            "actions" to result.actions.map { it.name.lowercase() },
            "notes" to
                result.notes.map { mapOf("type" to it.type.name.lowercase(), "value" to it.value) },
            "form" to
                result.form?.let { form ->
                    mapOf(
                        "title" to form.title,
                        "instructions" to form.instructions,
                        "fields" to
                            form.fields
                                .filterNot { it.blocked }
                                .map { field ->
                                    mapOf(
                                        "name" to field.name,
                                        "label" to field.label,
                                        "type" to field.type.name.lowercase(),
                                        "required" to field.required,
                                        "values" to field.values,
                                        "options" to
                                            field.options.map {
                                                mapOf("label" to it.label, "value" to it.value)
                                            },
                                    )
                                },
                    )
                },
        )
}

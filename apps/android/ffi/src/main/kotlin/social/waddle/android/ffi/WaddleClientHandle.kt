package social.waddle.android.ffi

import kotlinx.coroutines.flow.SharedFlow
import uniffi.waddle_xmpp_client.WaddleAvatar
import uniffi.waddle_xmpp_client.WaddleClient
import uniffi.waddle_xmpp_client.WaddleConfig
import uniffi.waddle_xmpp_client.WaddleMamPage
import uniffi.waddle_xmpp_client.WaddleRoom
import uniffi.waddle_xmpp_client.WaddleSendOptions
import uniffi.waddle_xmpp_client.WaddleUploadSlot

/**
 * Process-lifetime owner of a single [WaddleClient] and its
 * [WaddleEventBus]. The pairing is intentional: the Rust client holds a
 * strong reference to the listener for its entire lifetime, so the two
 * must share an owner.
 *
 * Construct exactly once per signed-in session, anchor in the
 * application-scoped DI graph, and never let a ViewModel hold the
 * reference directly.
 */
public class WaddleClientHandle(config: WaddleConfig) {
    private val bus: WaddleEventBus = WaddleEventBus()
    private val client: WaddleClient = WaddleClient(config, bus)

    public val events: SharedFlow<WaddleEvent> get() = bus.events

    public suspend fun connect(): Unit = client.connect()

    public suspend fun disconnect(): Unit = client.disconnect()

    public suspend fun sendChatMessage(
        peerJid: String,
        body: String,
        options: WaddleSendOptions? = null,
    ): Unit = client.sendChatMessage(peerJid, body, options)

    public suspend fun sendGroupchatMessage(
        roomJid: String,
        body: String,
        options: WaddleSendOptions? = null,
    ): Unit = client.sendGroupchatMessage(roomJid, body, options)

    public suspend fun joinRoom(roomJid: String, nick: String): Unit =
        client.joinRoom(roomJid, nick)

    public suspend fun leaveRoom(roomJid: String, nick: String): Unit =
        client.leaveRoom(roomJid, nick)

    public suspend fun listRooms(): List<WaddleRoom> = client.listRooms()

    public suspend fun fetchRoomHistory(
        roomJid: String,
        maxMessages: UInt,
        beforeId: String? = null,
    ): WaddleMamPage = client.fetchRoomHistory(roomJid, maxMessages, beforeId)

    public suspend fun fetchDmHistory(
        peerJid: String,
        maxMessages: UInt,
        beforeId: String? = null,
    ): WaddleMamPage = client.fetchDmHistory(peerJid, maxMessages, beforeId)

    public suspend fun sendPresence(status: String? = null, show: String? = null): Unit =
        client.sendPresence(status, show)

    public suspend fun requestAvatar(jid: String): WaddleAvatar? = client.requestAvatar(jid)

    public suspend fun discoverUploadService(): String? = client.discoverUploadService()

    public suspend fun requestUploadSlot(
        serviceJid: String,
        filename: String,
        size: ULong,
        contentType: String,
    ): WaddleUploadSlot? = client.requestUploadSlot(serviceJid, filename, size, contentType)
}

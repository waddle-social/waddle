package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleException
import social.waddle.client.ffi.WaddlePlanningItem
import social.waddle.client.ffi.WaddlePlanningKind

/** The operation that failed; creating a room and saving its details are distinct. */
enum class EventPlanningStage { CREATE_ROOM, SAVE_ITEM }

enum class EventPlanningFailureReason {
    NOT_CONNECTED, SESSION_CHANGED, PERMISSION_DENIED, UNSUPPORTED, UNAVAILABLE, INVALID_INPUT,
    NOT_FOUND, CONFLICT, SERVER_BUSY, TRANSPORT, TIMEOUT, MALFORMED_REPLY, UNEXPECTED,
}

/** Preserve the native error rather than turning every server refusal into offline state. */
data class EventPlanningFailure(
    val stage: EventPlanningStage,
    val reason: EventPlanningFailureReason,
    val nativeError: WaddleException? = null,
)

/** Opaque authority for one exact login, including same-account relogins. */
class EventPlanningOwner internal constructor(internal val lease: ActiveSession.OwnerLease) {
    fun sameSessionAs(other: EventPlanningOwner?): Boolean = other?.lease == lease
}

/** A created room that can be retried only by the account attempt that created it. */
class EventPlanningRetry internal constructor(
    val roomJid: String,
    internal val owner: EventPlanningOwner,
)

sealed interface EventPlanningResult {
    data class Saved(val roomJid: String, val item: WaddlePlanningItem) : EventPlanningResult
    data class Failed(
        val failure: EventPlanningFailure,
        val retry: EventPlanningRetry? = null,
    ) : EventPlanningResult
}

/** Keep an entire create-and-publish operation on one fenced client. Never nest gateways. */
internal class EventPlanningRequests(private val active: ActiveSession) {
    fun ownerFor(expectedSelf: String): EventPlanningOwner? = active.captureOwnerLease()
        ?.takeIf { it.ownerBareJid == expectedSelf.substringBefore('/') }
        ?.let(::EventPlanningOwner)

    suspend fun applyIfCurrent(owner: EventPlanningOwner?, action: () -> Unit): Boolean =
        owner != null && active.applyIfCurrent(owner.lease, action)

    suspend fun create(
        owner: EventPlanningOwner?,
        metadata: WaddlePlanningItem,
        retry: EventPlanningRetry? = null,
        onRoomCreated: (EventPlanningRetry) -> Unit = {},
    ): EventPlanningResult {
        var stage = if (retry == null) EventPlanningStage.CREATE_ROOM else EventPlanningStage.SAVE_ITEM
        if (owner == null) return failed(stage, EventPlanningFailureReason.NOT_CONNECTED, retry)
        if (retry != null && retry.owner.lease != owner.lease) {
            return failed(stage, EventPlanningFailureReason.SESSION_CHANGED, retry)
        }
        var created = retry
        return execute(owner, { stage }, { created }) { client ->
            val room = created?.roomJid ?: client.createEventRoom(metadata.title, true).also {
                created = EventPlanningRetry(it, owner)
                // Retain the room before the next suspension, including a cancelled publish.
                onRoomCreated(checkNotNull(created))
            }
            stage = EventPlanningStage.SAVE_ITEM
            currentCoroutineContext().ensureActive()
            val input = metadata.copy(id = "event", kind = WaddlePlanningKind.EVENT)
            val saved = client.eventPlanningPublish(room, authored(input, owner))
            EventPlanningResult.Saved(room, saved)
        }
    }

    suspend fun save(
        owner: EventPlanningOwner?,
        room: String,
        item: WaddlePlanningItem,
    ): EventPlanningResult {
        val stage = EventPlanningStage.SAVE_ITEM
        if (owner == null) return failed(stage, EventPlanningFailureReason.NOT_CONNECTED)
        return execute(owner, { stage }, { null }) { client ->
            EventPlanningResult.Saved(room, client.eventPlanningPublish(room, authored(item, owner)))
        }
    }

    private suspend fun execute(
        owner: EventPlanningOwner,
        stage: () -> EventPlanningStage,
        retry: () -> EventPlanningRetry?,
        operation: suspend (social.waddle.client.ffi.WaddleClientInterface) -> EventPlanningResult,
    ): EventPlanningResult = try {
        val invocation = active.invokeIfCurrent(owner.lease) { client ->
            currentCoroutineContext().ensureActive()
            operation(client)
        }
        when (invocation) {
            ActiveSession.LeaseInvocation.Stale ->
                failed(stage(), EventPlanningFailureReason.SESSION_CHANGED, retry())
            ActiveSession.LeaseInvocation.NotConnected ->
                failed(stage(), EventPlanningFailureReason.NOT_CONNECTED, retry())
            is ActiveSession.LeaseInvocation.Completed -> invocation.value
        }
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: WaddleException) {
        EventPlanningResult.Failed(EventPlanningFailure(stage(), reason(error), error), retry())
    } catch (_: Exception) {
        failed(stage(), EventPlanningFailureReason.UNEXPECTED, retry())
    }

    private fun authored(item: WaddlePlanningItem, owner: EventPlanningOwner): WaddlePlanningItem {
        val self = owner.lease.ownerBareJid
        val id = when (item.kind) {
            WaddlePlanningKind.EVENT -> "event"
            WaddlePlanningKind.ATTENDANCE -> "attendance:$self"
            WaddlePlanningKind.VOTE -> "vote:${item.subject}:$self"
            else -> item.id
        }
        return item.copy(id = id, author = self)
    }

    private fun failed(
        stage: EventPlanningStage,
        reason: EventPlanningFailureReason,
        retry: EventPlanningRetry? = null,
    ) = EventPlanningResult.Failed(EventPlanningFailure(stage, reason), retry)

    private fun reason(error: WaddleException): EventPlanningFailureReason = when (error) {
        is WaddleException.NotConnected -> EventPlanningFailureReason.NOT_CONNECTED
        is WaddleException.InvalidArgument, is WaddleException.InvalidJid -> EventPlanningFailureReason.INVALID_INPUT
        is WaddleException.Transport -> EventPlanningFailureReason.TRANSPORT
        is WaddleException.Timeout -> EventPlanningFailureReason.TIMEOUT
        is WaddleException.MalformedResponse, is WaddleException.UntrustedReply ->
            EventPlanningFailureReason.MALFORMED_REPLY
        is WaddleException.Stanza -> stanzaReason(error.condition)
        else -> EventPlanningFailureReason.UNEXPECTED
    }

    private fun stanzaReason(condition: String): EventPlanningFailureReason = when (condition) {
        "forbidden", "not-allowed", "not-authorized", "registration-required" ->
            EventPlanningFailureReason.PERMISSION_DENIED
        "feature-not-implemented" -> EventPlanningFailureReason.UNSUPPORTED
        "service-unavailable" -> EventPlanningFailureReason.UNAVAILABLE
        "bad-request", "not-acceptable", "jid-malformed" -> EventPlanningFailureReason.INVALID_INPUT
        "item-not-found" -> EventPlanningFailureReason.NOT_FOUND
        "conflict" -> EventPlanningFailureReason.CONFLICT
        "resource-constraint" -> EventPlanningFailureReason.SERVER_BUSY
        else -> EventPlanningFailureReason.UNEXPECTED
    }
}

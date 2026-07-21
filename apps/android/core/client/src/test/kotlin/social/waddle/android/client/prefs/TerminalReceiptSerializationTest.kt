package social.waddle.android.client.prefs

import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.UUID

class TerminalReceiptSerializationTest {
    private val json = Json {
        encodeDefaults = true
        ignoreUnknownKeys = true
    }

    @Test
    fun `terminal receipt round trips every claimant and receipt state`() {
        val states = listOf(
            TerminalReceiptState.Pending(TerminalReceiptClaimState.Unclaimed, listOf(effect())),
            claimedPending(
                TerminalReceiptClaimant.Worker(
                    lifecycleGeneration = LifecycleGeneration(uuid("lifecycle-drain")),
                    kind = TerminalReceiptWorkerKind.OUTBOUND_DRAIN,
                    workerGeneration = WorkerGeneration(uuid("worker-drain")),
                ),
            ),
            claimedPending(
                TerminalReceiptClaimant.Worker(
                    lifecycleGeneration = LifecycleGeneration(uuid("lifecycle-terminal")),
                    kind = TerminalReceiptWorkerKind.DELIVERY_TERMINAL,
                    workerGeneration = WorkerGeneration(uuid("worker-terminal")),
                ),
            ),
            claimedPending(
                TerminalReceiptClaimant.Finalizer(
                    lifecycleGeneration = LifecycleGeneration(uuid("lifecycle-finalizer")),
                    finalizerGeneration = FinalizerGeneration(uuid("finalizer")),
                ),
            ),
            claimedPending(TerminalReceiptClaimant.BootstrapProcess),
            acknowledged(),
        )
        states.forEachIndexed { index, state ->
            val receipt = receipt("state-$index", state)
            assertEquals(receipt, json.decodeFromString<TerminalReceipt>(json.encodeToString(receipt)))
        }
    }

    @Test
    fun `terminal receipt domain rejects invalid typed identities and bindings`() {
        assertInvalid { DeliveryOwnerBareJid(" ") }
        assertInvalid { TerminalReceiptId("not-a-uuid") }
        assertInvalid { TerminalClaimId("not-a-uuid") }
        assertInvalid { ProcessEpoch("not-a-uuid") }
        assertInvalid { LifecycleGeneration("not-a-uuid") }
        assertInvalid { WorkerGeneration("not-a-uuid") }
        assertInvalid { FinalizerGeneration("not-a-uuid") }
        assertInvalid { TerminalReceiptId("1-1-1-1-1") }
        assertInvalid {
            TerminalReceiptState.Pending(TerminalReceiptClaimState.Unclaimed, emptyList())
        }
        val duplicate = effect()
        assertInvalid {
            TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                listOf(duplicate, duplicate),
            )
        }
        assertInvalid {
            TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                listOf(
                    duplicate,
                    TerminalReceiptEffect.Acknowledged(
                        DeliveryCallbackRef(duplicate.row.identity, attempt("other-callback")),
                        duplicate.row,
                    ),
                ),
            )
        }
        assertInvalid {
            TerminalReceipt(
                owner = DeliveryOwnerBareJid(OTHER_OWNER),
                attempt = attempt(),
                id = TerminalReceiptId(uuid("mismatch")),
                originProcessEpoch = ProcessEpoch(uuid("epoch")),
                preparedAtMillis = 1,
                state = acknowledged(),
            )
        }
        assertInvalid {
            TerminalReceipt(
                owner = DeliveryOwnerBareJid(OWNER),
                attempt = attempt(),
                id = TerminalReceiptId(uuid("negative")),
                originProcessEpoch = ProcessEpoch(uuid("epoch-negative")),
                preparedAtMillis = -1,
                state = acknowledged(),
            )
        }
        val validRow = row()
        assertInvalid {
            TerminalReceiptEffect.Acknowledged(
                callback = DeliveryCallbackRef(row("other").identity, attempt()),
                row = validRow,
            )
        }
        val foreignRow = validRow.copy(ownerBareJid = OTHER_OWNER)
        assertInvalid {
            receipt(
                "row-owner",
                TerminalReceiptState.Pending(
                    TerminalReceiptClaimState.Unclaimed,
                    listOf(
                        TerminalReceiptEffect.Acknowledged(
                            DeliveryCallbackRef(foreignRow.identity, attempt()),
                            foreignRow,
                        ),
                    ),
                ),
            )
        }
        assertInvalid {
            receipt(
                "callback-attempt",
                TerminalReceiptState.Pending(
                    TerminalReceiptClaimState.Unclaimed,
                    listOf(
                        TerminalReceiptEffect.Acknowledged(
                            DeliveryCallbackRef(validRow.identity, attempt("other-attempt")),
                            validRow,
                        ),
                    ),
                ),
            )
        }
    }

    @Test
    fun `malformed durable receipt json rejects receipt bindings`() {
        val receipt = receipt(
            "json",
            TerminalReceiptState.Pending(TerminalReceiptClaimState.Unclaimed, listOf(effect())),
        )
        val encoded = json.encodeToString(receipt)
        assertDecodeInvalid(encoded.replaceFirst("\"clientStanzaId\":\"terminal-row\"", "\"clientStanzaId\":\"wrong-row\""))
        val callbackOwnerChanged = encoded.replaceFirst(
            "\"row\":{\"ownerBareJid\":\"$OWNER\"",
            "\"row\":{\"ownerBareJid\":\"$OTHER_OWNER\"",
        )
        assertDecodeInvalid(
            callbackOwnerChanged.replaceFirst(
                "\"row\":{\"ownerBareJid\":\"$OWNER\"",
                "\"row\":{\"ownerBareJid\":\"$OTHER_OWNER\"",
            ),
        )
        val receiptAttempt = "\"attemptId\":\"${uuid("receipt-attempt")}\""
        val callbackAttemptOffset = encoded.lastIndexOf(receiptAttempt)
        assertTrue(callbackAttemptOffset >= 0)
        assertDecodeInvalid(
            encoded.substring(0, callbackAttemptOffset) +
                "\"attemptId\":\"${uuid("different-attempt")}\"" +
                encoded.substring(callbackAttemptOffset + receiptAttempt.length),
        )
        assertDecodeInvalid(encoded.replace("\"preparedAtMillis\":1", "\"preparedAtMillis\":-1"))
        val distinctEffects = receipt(
            "duplicate-effects",
            TerminalReceiptState.Pending(
                TerminalReceiptClaimState.Unclaimed,
                listOf(effect("terminal-row-a", 1), effect("terminal-row-b", 2)),
            ),
        )
        assertDecodeInvalid(
            json.encodeToString(distinctEffects)
                .replace("terminal-row-b", "terminal-row-a")
                .replace(
                    uuid("incarnation-terminal-row-b"),
                    uuid("incarnation-terminal-row-a"),
                ),
        )

        val journal = DeliveryJournal(
            activeOwnerBareJid = OWNER,
            owners = mapOf(OWNER to DeliveryOwnerJournal(terminalReceipt = receipt("ack", acknowledged()))),
        )
        assertInvalid {
            json.decodeFromString<DeliveryJournal>(
                json.encodeToString(journal).replaceFirst("\"$OWNER\":{", "\"$OTHER_OWNER\":{"),
            )
        }
    }

    private fun claimedPending(claimant: TerminalReceiptClaimant): TerminalReceiptState.Pending =
        TerminalReceiptState.Pending(
            claim = TerminalReceiptClaimState.Claimed(
                id = TerminalClaimId(uuid("claim-${claimant::class.simpleName}")),
                claimant = claimant,
                processEpoch = ProcessEpoch(uuid("epoch-${claimant::class.simpleName}")),
            ),
            effects = listOf(effect()),
        )

    private fun acknowledged(): TerminalReceiptState.Acknowledged = TerminalReceiptState.Acknowledged(
        TerminalReceiptClaimState.Claimed(
            TerminalClaimId(uuid("ack-claim")),
            TerminalReceiptClaimant.BootstrapProcess,
            ProcessEpoch(uuid("ack-epoch")),
        ),
    )

    private fun assertDecodeInvalid(encoded: String) {
        assertInvalid { json.decodeFromString<TerminalReceipt>(encoded) }
    }

    private fun assertInvalid(block: () -> Unit) {
        try {
            block()
            throw AssertionError("expected IllegalArgumentException")
        } catch (_: IllegalArgumentException) {
            Unit
        }
    }

    private fun receipt(id: String, state: TerminalReceiptState): TerminalReceipt = TerminalReceipt(
        owner = DeliveryOwnerBareJid(OWNER),
        attempt = attempt(),
        id = TerminalReceiptId(uuid(id)),
        originProcessEpoch = ProcessEpoch(uuid("origin-$id")),
        preparedAtMillis = 1,
        state = state,
    )

    private fun effect(id: String = "terminal-row", sequence: Long = 1): TerminalReceiptEffect {
        val row = row(id, sequence)
        return TerminalReceiptEffect.Acknowledged(
            callback = DeliveryCallbackRef(row.identity, attempt()),
            row = row,
        )
    }

    private fun row(id: String = "terminal-row", sequence: Long = 1): QueuedOutboundMessage = QueuedOutboundDraft.create(
        ownerBareJid = OWNER,
        clientStanzaId = id,
        enqueuedAtMillis = sequence,
        payload = QueuedOutboundPayload(
            target = QueuedOutboundTarget.Chat("peer@waddle.test"),
            content = QueuedOutboundContent("body"),
        ),
        incarnation = DeliveryIncarnation(uuid("incarnation-$id")),
    ).persisted(sequence, OutboundOwnership.Ready)

    private fun attempt(seed: String = "receipt-attempt"): DeliveryAttemptRef = DeliveryAttemptRef(
        ownerBareJid = OWNER,
        attemptId = DeliveryAttemptId(uuid(seed)),
        nativeGeneration = NativeConnectionGeneration(1u),
    )

    private fun uuid(seed: String): String = UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object {
        const val OWNER = "icepuma@waddle.test"
        const val OTHER_OWNER = "other@waddle.test"
    }
}

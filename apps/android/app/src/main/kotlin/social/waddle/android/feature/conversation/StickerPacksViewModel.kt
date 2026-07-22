package social.waddle.android.feature.conversation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.VerbResult
import social.waddle.android.viewModelFactoryOf

/** Phase of the current create-pack attempt, survives recreation. */
sealed interface CreatePackPhase {
    data object Idle : CreatePackPhase

    data class Creating(val done: Int, val total: Int) : CreatePackPhase

    data class Failed(val result: CreateStickerPackResult) : CreatePackPhase

    /** Sticky until the sheet consumes it — a config change between
     * the publish Ok and the sheet's next composition must not lose
     * the success. */
    data object Succeeded : CreatePackPhase
}

/**
 * ViewModel-scoped sticker-pack mutations: the create pipeline
 * ([StickerPackCreator]: process → upload → publish) and pack removal
 * both outlive their launching sheet — a rotation mid-upload or a
 * dismiss right after the remove confirm must not cancel the attempt
 * and strand a half-created pack or an un-reconciled store.
 * The sheets own only form state; every attempt lives here.
 */
class StickerPacksViewModel(
    private val create: suspend (
        name: String,
        summary: String?,
        images: List<StickerImageInput>,
        onProgress: (Int, Int) -> Unit,
    ) -> CreateStickerPackResult,
    private val remove: suspend (packId: String) -> VerbResult,
) : ViewModel() {
    private val _createPhase = MutableStateFlow<CreatePackPhase>(CreatePackPhase.Idle)

    /** The current create attempt as observable, recreation-safe state. */
    val createPhase: StateFlow<CreatePackPhase> = _createPhase.asStateFlow()

    private val _removeFailure = MutableStateFlow<VerbResult.Failure?>(null)

    /**
     * The most recent refused removal, sticky until consumed: a
     * subscriber-less window (sheet closed, rotation mid-IQ) must not
     * silently drop the only signal that the pack is still there.
     */
    val removeFailure: StateFlow<VerbResult.Failure?> = _removeFailure.asStateFlow()

    /**
     * The SHOWN failure was surfaced; clear it — compare-and-set so a
     * DIFFERENT failure landing mid-display survives and re-emits.
     */
    fun consumeRemoveFailure(shown: VerbResult.Failure) {
        _removeFailure.compareAndSet(shown, null)
    }

    /**
     * Clear a terminal create phase so a reopened sheet starts fresh —
     * a stale Failed banner over an empty form is last attempt's news.
     * A Creating phase stays: the pipeline is still running.
     */
    fun resetCreatePhase() {
        if (_createPhase.value !is CreatePackPhase.Creating) {
            _createPhase.value = CreatePackPhase.Idle
        }
    }

    /** Launch one create attempt; a no-op while one is in flight. */
    fun createPack(name: String, summary: String?, images: List<StickerImageInput>) {
        if (_createPhase.value is CreatePackPhase.Creating) return
        _createPhase.value = CreatePackPhase.Creating(0, images.size)
        viewModelScope.launch {
            val result = try {
                create(name, summary, images) { done, total ->
                    _createPhase.value = CreatePackPhase.Creating(done, total)
                }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                CreateStickerPackResult.Failed
            }
            _createPhase.value = if (result == CreateStickerPackResult.Ok) {
                CreatePackPhase.Succeeded
            } else {
                CreatePackPhase.Failed(result)
            }
        }
    }

    /** The sheet acted on [CreatePackPhase.Succeeded]: back to idle. */
    fun consumeCreateSuccess() {
        if (_createPhase.value == CreatePackPhase.Succeeded) {
            _createPhase.value = CreatePackPhase.Idle
        }
    }

    /**
     * Retract [packId]; the session cache reconciles inside the verb
     * on Ok (idempotently — an already-gone pack still reconciles),
     * refusals surface via [removeFailure].
     */
    fun removePack(packId: String) {
        viewModelScope.launch {
            val result = try {
                remove(packId)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                VerbResult.Rejected
            }
            if (result is VerbResult.Failure) _removeFailure.value = result
        }
    }

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            StickerPacksViewModel(
                create = graph.stickerPackCreator::create,
                remove = graph.sessionManager::removeStickerPack,
            )
        }
    }
}

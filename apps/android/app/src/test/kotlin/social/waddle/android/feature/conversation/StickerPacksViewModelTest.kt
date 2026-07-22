package social.waddle.android.feature.conversation

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.VerbResult

/**
 * The VM-scoped create/remove attempts: observable phase transitions
 * (progress → success/failure), sticky success until consumed, the
 * in-flight guard, and remove refusals surfacing as failure events —
 * the state a recreated sheet resumes from after a config change.
 *
 * Image inputs stay empty here: `android.net.Uri` is unmockable in
 * plain unit tests, and the VM reads only the list's size (the initial
 * progress seed) — the fake pipeline drives progress itself.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class StickerPacksViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `create reports progress and lands on sticky success`() = runTest(dispatcher.scheduler) {
        val gate = CompletableDeferred<CreateStickerPackResult>()
        val phases = mutableListOf<CreatePackPhase>()
        val viewModel = StickerPacksViewModel(
            create = { _, _, _, onProgress ->
                onProgress(1, 2)
                onProgress(2, 2)
                gate.await()
            },
            remove = { VerbResult.Ok },
        )

        viewModel.createPack("Penguins", null, emptyList())
        phases += viewModel.createPhase.value
        runCurrent()
        phases += viewModel.createPhase.value

        // In-flight guard: a second tap must not restart the pipeline.
        viewModel.createPack("Penguins", null, emptyList())
        runCurrent()
        assertEquals(CreatePackPhase.Creating(2, 2), viewModel.createPhase.value)

        gate.complete(CreateStickerPackResult.Ok)
        runCurrent()
        assertEquals(CreatePackPhase.Succeeded, viewModel.createPhase.value)
        assertEquals(
            listOf<CreatePackPhase>(CreatePackPhase.Creating(0, 0), CreatePackPhase.Creating(2, 2)),
            phases,
        )

        // Sticky until the sheet consumes it, then idle for the next attempt.
        viewModel.consumeCreateSuccess()
        assertEquals(CreatePackPhase.Idle, viewModel.createPhase.value)
    }

    @Test
    fun `create failure surfaces the typed result`() = runTest(dispatcher.scheduler) {
        val viewModel = StickerPacksViewModel(
            create = { _, _, _, _ -> CreateStickerPackResult.NotConnected },
            remove = { VerbResult.Ok },
        )

        viewModel.createPack("Penguins", null, emptyList())
        runCurrent()

        assertEquals(
            CreatePackPhase.Failed(CreateStickerPackResult.NotConnected),
            viewModel.createPhase.value,
        )
        // Consuming success is a no-op on failure: the sheet keeps the error.
        viewModel.consumeCreateSuccess()
        assertTrue(viewModel.createPhase.value is CreatePackPhase.Failed)
    }

    @Test
    fun `a throwing pipeline collapses to the failed phase`() = runTest(dispatcher.scheduler) {
        val viewModel = StickerPacksViewModel(
            create = { _, _, _, _ -> throw IllegalStateException("boom") },
            remove = { VerbResult.Ok },
        )

        viewModel.createPack("Penguins", null, emptyList())
        runCurrent()

        assertEquals(
            CreatePackPhase.Failed(CreateStickerPackResult.Failed),
            viewModel.createPhase.value,
        )
    }

    @Test
    fun `remove refusals surface as failure events`() = runTest(dispatcher.scheduler) {
        val removed = mutableListOf<String>()
        var result: VerbResult = VerbResult.Ok
        val viewModel = StickerPacksViewModel(
            create = { _, _, _, _ -> CreateStickerPackResult.Ok },
            remove = { packId ->
                removed += packId
                result
            },
        )
        viewModel.removePack("pack-1")
        runCurrent()
        assertEquals(listOf("pack-1"), removed)
        assertNull(viewModel.removeFailure.value)

        result = VerbResult.Rejected
        viewModel.removePack("pack-2")
        runCurrent()
        // Sticky until consumed: a subscriber-less window (sheet
        // closed, rotation mid-IQ) must not drop the signal.
        assertEquals(VerbResult.Rejected, viewModel.removeFailure.value)
        viewModel.consumeRemoveFailure()
        assertNull(viewModel.removeFailure.value)
    }

    @Test
    fun `a throwing remove is a rejection`() = runTest(dispatcher.scheduler) {
        val viewModel = StickerPacksViewModel(
            create = { _, _, _, _ -> CreateStickerPackResult.Ok },
            remove = { throw IllegalStateException("boom") },
        )
        viewModel.removePack("pack-1")
        runCurrent()

        assertEquals(VerbResult.Rejected, viewModel.removeFailure.value)
    }

    @Test
    fun `resetCreatePhase clears a terminal failure but never a live attempt`() = runTest(dispatcher.scheduler) {
        val gate = CompletableDeferred<CreateStickerPackResult>()
        val viewModel = StickerPacksViewModel(
            create = { _, _, _, _ -> gate.await() },
            remove = { VerbResult.Ok },
        )

        viewModel.createPack("pack", null, emptyList())
        runCurrent()
        // Mid-flight: reset must not clobber the running attempt.
        viewModel.resetCreatePhase()
        assertTrue(viewModel.createPhase.value is CreatePackPhase.Creating)

        gate.complete(CreateStickerPackResult.Failed)
        runCurrent()
        assertTrue(viewModel.createPhase.value is CreatePackPhase.Failed)
        // A dismissed sheet consumes the stale banner.
        viewModel.resetCreatePhase()
        assertEquals(CreatePackPhase.Idle, viewModel.createPhase.value)
    }
}

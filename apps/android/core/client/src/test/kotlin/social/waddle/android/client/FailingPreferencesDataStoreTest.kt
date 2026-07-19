package social.waddle.android.client

import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.core.mutablePreferencesOf
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class FailingPreferencesDataStoreTest {
    @Test
    fun `transition one-shot ignores unrelated matching state and consumes once`() = runTest {
        val store = FailingPreferencesDataStore()
        val target = intPreferencesKey("target")
        val unrelated = intPreferencesKey("unrelated")
        var calls = 0
        store.updateData { mutablePreferencesOf(target to 2, unrelated to 0) }
        store.installAfterCommitReturnsOnceWhen(
            matches = { before, after -> before[target] != 2 && after[target] == 2 },
        ) { calls += 1 }
        store.updateData { mutablePreferencesOf(target to 2, unrelated to 1) }
        assertEquals(0, calls)
        store.updateData { mutablePreferencesOf(target to 1, unrelated to 1) }
        assertEquals(0, calls)
        store.updateData { mutablePreferencesOf(target to 2, unrelated to 1) }
        assertEquals(1, calls)
        store.updateData { mutablePreferencesOf(target to 1, unrelated to 1) }
        store.updateData { mutablePreferencesOf(target to 2, unrelated to 1) }
        assertEquals(1, calls)
    }

    @Test
    @OptIn(ExperimentalCoroutinesApi::class)
    fun `transition one-shot is reserved before a matching hook completes`() = runTest {
        val store = FailingPreferencesDataStore()
        val target = intPreferencesKey("concurrent-target")
        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val calls = AtomicInteger()
        store.updateData { mutablePreferencesOf(target to 0) }
        store.installAfterCommitReturnsOnceWhen(
            matches = { _, after -> after[target] == 1 },
        ) {
            calls.incrementAndGet()
            entered.complete(Unit)
            release.await()
        }
        val first = async {
            store.updateData { mutablePreferencesOf(target to 1) }
        }
        entered.await()
        val second = async {
            store.updateData { mutablePreferencesOf(target to 1) }
        }
        try {
            runCurrent()
            assertTrue("second matching commit must not wait for the hook", second.isCompleted)
            second.await()
            assertEquals(1, calls.get())
        } finally {
            release.complete(Unit)
            first.await()
            second.await()
        }
        assertEquals(1, calls.get())
    }

    @Test
    fun `repeatable after-commit hook runs for every commit`() = runTest {
        val store = FailingPreferencesDataStore()
        val key = intPreferencesKey("repeatable")
        var calls = 0
        store.afterCommitReturns = { calls += 1 }
        store.updateData { mutablePreferencesOf(key to 1) }
        store.updateData { mutablePreferencesOf(key to 2) }
        assertEquals(2, calls)
    }
}

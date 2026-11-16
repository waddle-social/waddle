extension SessionCoordinator {
    /// A transport verb may ignore cancellation, so teardown races an
    /// independent deadline instead of awaiting a cancelled child task.
    func stopCalls() async {
        if calls.state != .idle {
            let (completed, continuation) = AsyncStream<Void>.makeStream()
            let teardown = Task { [calls] in
                await calls.hangUp()
                continuation.yield(())
                continuation.finish()
            }
            let deadline = Task {
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
                continuation.yield(())
                continuation.finish()
            }
            var iterator = completed.makeAsyncIterator()
            _ = await iterator.next()
            teardown.cancel()
            deadline.cancel()
            continuation.finish()
        }
        // Fence suspended sends before disconnect can suspend too; a late
        // terminate must never send a finish on the next signed-in session.
        calls.clear()
    }
}

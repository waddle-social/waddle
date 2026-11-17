/// A one-shot result survives teardown after resolution; pending waits always finish.
/// Every observer shares the result, including timeout and cancellation.
@MainActor final class GroupCallWaiter<Value: Sendable> {
    private enum Result { case pending, finished(Value?) }
    private var result: Result = .pending
    private var continuations: [CheckedContinuation<Value?, Never>] = []
    private var timeoutTasks: [Task<Void, Never>] = []

    func resolve(_ value: Value) { finish(value) }
    func cancel() { finish(nil) }

    func wait(timeout: Duration) async -> Value? {
        await withTaskCancellationHandler {
            if Task.isCancelled { cancel() }
            if case .finished(let value) = result { return value }
            return await withCheckedContinuation { continuation in
                continuations.append(continuation)
                timeoutTasks.append(Task { [weak self] in
                    do { try await Task.sleep(for: timeout) } catch { return }
                    guard !Task.isCancelled else { return }
                    self?.cancel()
                })
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.cancel() }
        }
    }

    private func finish(_ value: Value?) {
        guard case .pending = result else { return }
        result = .finished(value)
        timeoutTasks.forEach { $0.cancel() }
        timeoutTasks.removeAll()
        let waiting = continuations
        continuations.removeAll()
        for continuation in waiting { continuation.resume(returning: value) }
    }
}

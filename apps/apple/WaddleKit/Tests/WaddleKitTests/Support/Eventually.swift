import Foundation

/// Polls `condition` until it holds or `timeout` passes. Tests that wait on
/// the coordinator's own timers use this rather than a fixed sleep, so a
/// busy runner delays them instead of failing them.
@MainActor
func eventually(timeout: TimeInterval = 5, _ condition: () -> Bool) async {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition(), Date() < deadline {
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
}

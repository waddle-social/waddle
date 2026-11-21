import Foundation
import Testing
@testable import WaddleKit

/// ADR-010: a device that sees no input turns Away after 10 minutes and
/// Extended Away after 30, stamped with the moment input stopped.
@Suite("Auto-away")
struct AutoAwayTests {
    private let lastInput = Date(timeIntervalSince1970: 1_700_000_000)

    private func after(_ minutes: Double) -> AutoAway {
        AutoAway(now: lastInput.addingTimeInterval(minutes * 60), lastActive: lastInput)
    }

    @Test func aDeviceInUseIsAvailableWithNoIdleStamp() {
        #expect(after(0) == AutoAway(availability: .available, idleSince: nil))
        #expect(after(9.9) == AutoAway(availability: .available, idleSince: nil))
    }

    @Test func tenMinutesWithoutInputIsAwaySinceTheLastInput() {
        #expect(after(10) == AutoAway(availability: .away, idleSince: lastInput))
        #expect(after(29.9) == AutoAway(availability: .away, idleSince: lastInput))
    }

    @Test func thirtyMinutesWithoutInputIsExtendedAwaySinceTheLastInput() {
        #expect(after(30) == AutoAway(availability: .extendedAway, idleSince: lastInput))
        #expect(after(600) == AutoAway(availability: .extendedAway, idleSince: lastInput))
    }

    /// A clock that steps back must not read as idle.
    @Test func aLastInputInTheFutureIsAvailable() {
        #expect(after(-5) == AutoAway(availability: .available, idleSince: nil))
    }

    // MARK: What goes out

    @Test func automaticModeSendsWhatAutoAwaySays() {
        let idle = after(12)
        #expect(OwnPresence(mode: .automatic, autoAway: idle) == OwnPresence(availability: .away, idleSince: lastInput))
    }

    /// A status picked by hand suspends auto-away, so it never carries an
    /// idle stamp, however long the device has been idle.
    @Test(arguments: [
        (ManualStatus.available, Availability.available),
        (.chat, .chat),
        (.away, .away),
        (.doNotDisturb, .doNotDisturb),
    ])
    func aPickedStatusIsSentAsItIsWithNoIdleStamp(status: ManualStatus, availability: Availability) {
        #expect(OwnPresence(mode: .manual(status), autoAway: after(45)) == OwnPresence(availability: availability, idleSince: nil))
    }
}

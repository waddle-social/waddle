import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// ADR-010: a status picked by hand is the account's, stored on the server
/// and the same on every device; without one, each device's own idle time
/// decides what it shows.
@MainActor
@Suite("Presence mode")
struct PresenceModeTests {
    /// The test's clock: the coordinator reads the time from here.
    final class Clock {
        var now = date(0)
        func advance(minutes: Double) { now = now.addingTimeInterval(minutes * 60) }
    }

    private func session(stored: PresenceMode = .automatic) -> (SessionCoordinator, FakePort, Clock) {
        let port = FakePort()
        port.storedPresenceMode = .success(stored)
        let coordinator = SessionCoordinator(account: me, port: port)
        let clock = Clock()
        coordinator.now = { clock.now }
        coordinator.noteActivity()
        return (coordinator, port, clock)
    }

    private func connect(_ coordinator: SessionCoordinator, _ port: FakePort, again: Bool = false) async {
        if !again { coordinator.start() }
        port.emit(.connected)
        await eventually { coordinator.isSendReady }
        await coordinator.readyTask?.value
    }

    private func connected(stored: PresenceMode = .automatic) async -> (SessionCoordinator, FakePort, Clock) {
        let (coordinator, port, clock) = session(stored: stored)
        await connect(coordinator, port)
        return (coordinator, port, clock)
    }

    private func sent(_ availability: Availability, idleSince: Date? = nil, status: String? = nil) -> FakePort.PresenceSend {
        FakePort.PresenceSend(availability: availability, status: status, idleSince: idleSince)
    }

    // MARK: Connecting

    @Test func aDeviceWithNothingStoredIsAutomaticAndPublishesNothing() async {
        let (coordinator, port, _) = await connected()

        #expect(coordinator.status.presenceMode == .automatic)
        #expect(port.presenceSends == [sent(.available)])
        #expect(port.publishedPresenceModes.isEmpty)
        await coordinator.stop()
    }

    /// A new device starts Automatic. It takes the account's stored pick
    /// rather than writing its own default over it.
    @Test func aNewDeviceTakesTheStatusPickedOnAnotherDevice() async {
        let (coordinator, port, _) = await connected(stored: .manual(.doNotDisturb))

        #expect(coordinator.status.presenceMode == .manual(.doNotDisturb))
        #expect(coordinator.status.availability == .doNotDisturb)
        #expect(port.presenceSends.last == sent(.doNotDisturb))
        #expect(port.publishedPresenceModes.isEmpty)
        await coordinator.stop()
    }

    @Test func aStoredPickThatCannotBeReadLeavesTheDeviceAsItWas() async {
        let (coordinator, port, _) = session()
        port.storedPresenceMode = .failure(.timeout)
        await connect(coordinator, port)

        #expect(coordinator.status.presenceMode == .automatic)
        #expect(port.presenceSends == [sent(.available)])
        #expect(port.publishedPresenceModes.isEmpty)
        await coordinator.stop()
    }

    // MARK: Picking

    @Test func aPickedStatusIsShownAndStoredForTheOtherDevices() async {
        let (coordinator, port, _) = await connected()

        await coordinator.setPresenceMode(.manual(.away))

        #expect(coordinator.status.presenceMode == .manual(.away))
        #expect(coordinator.status.availability == .away)
        #expect(port.presenceSends.last == sent(.away))
        #expect(port.publishedPresenceModes == [.manual(.away)])
        await coordinator.stop()
    }

    /// Stored as Automatic, not removed, so the other devices hear of it.
    @Test func resettingToAutomaticIsStoredToo() async {
        let (coordinator, port, _) = await connected(stored: .manual(.doNotDisturb))

        await coordinator.setPresenceMode(.automatic)

        #expect(coordinator.status.availability == .available)
        #expect(port.presenceSends.last == sent(.available))
        #expect(port.publishedPresenceModes == [.automatic])
        await coordinator.stop()
    }

    @Test func pickingTheStatusAlreadyHeldStoresNothing() async {
        let (coordinator, port, _) = await connected(stored: .manual(.away))
        let sends = port.presenceSends.count

        await coordinator.setPresenceMode(.manual(.away))

        #expect(port.publishedPresenceModes.isEmpty)
        #expect(port.presenceSends.count == sends)
        await coordinator.stop()
    }

    /// A status chosen while offline is what the user wants now: it wins
    /// over the older stored one.
    @Test func aStatusPickedOfflineWinsOverTheStoredOneOnConnect() async {
        let (coordinator, port, _) = session(stored: .manual(.away))

        await coordinator.setPresenceMode(.manual(.doNotDisturb))
        #expect(port.publishedPresenceModes.isEmpty)
        await connect(coordinator, port)

        #expect(coordinator.status.presenceMode == .manual(.doNotDisturb))
        #expect(port.publishedPresenceModes == [.manual(.doNotDisturb)])
        #expect(port.presenceSends.last == sent(.doNotDisturb))
        await coordinator.stop()
    }

    @Test func aPickTheServerDidNotTakeIsStoredOnTheNextConnect() async {
        let (coordinator, port, _) = await connected()
        port.presenceModePublishResult = .failure(.timeout)
        await coordinator.setPresenceMode(.manual(.away))
        #expect(coordinator.status.presenceMode == .manual(.away))

        port.presenceModePublishResult = .success(())
        port.emit(.disconnected)
        await eventually { !coordinator.isSendReady }
        await connect(coordinator, port, again: true)

        #expect(coordinator.status.presenceMode == .manual(.away))
        #expect(port.publishedPresenceModes == [.manual(.away), .manual(.away)])
        await coordinator.stop()
    }

    /// The second pick is made while the first is on its way: both reach
    /// the server, in order, and the last one stands.
    @Test func aPickMadeWhileAnotherIsBeingStoredIsStoredAfterIt() async {
        let (coordinator, port, _) = await connected()
        port.holdsPresenceModePublishes = true

        async let first: Void = coordinator.setPresenceMode(.manual(.away))
        await eventually { port.heldPresenceModePublishes == 1 }
        await coordinator.setPresenceMode(.manual(.doNotDisturb))
        port.holdsPresenceModePublishes = false
        port.releasePresenceModePublishes()
        await first

        #expect(port.publishedPresenceModes == [.manual(.away), .manual(.doNotDisturb)])
        #expect(coordinator.status.presenceMode == .manual(.doNotDisturb))
        await coordinator.stop()
    }

    // MARK: Another device picks

    @Test func aStatusPickedOnAnotherDeviceIsShownHereAndNotStoredAgain() async {
        let (coordinator, port, _) = await connected()

        port.emit(.presenceModeChanged(.manual(.doNotDisturb)))
        await eventually { port.presenceSends.last == sent(.doNotDisturb) }

        #expect(coordinator.status.presenceMode == .manual(.doNotDisturb))
        #expect(port.presenceSends.last == sent(.doNotDisturb))
        #expect(port.publishedPresenceModes.isEmpty)
        await coordinator.stop()
    }

    @Test func hearingOfTheStatusAlreadyHeldChangesNothing() async {
        let (coordinator, port, _) = await connected()
        await coordinator.setPresenceMode(.manual(.away))
        let sends = port.presenceSends.count

        coordinator.handle(.presenceModeChanged(.manual(.away)))
        try? await Task.sleep(for: .milliseconds(50))

        #expect(port.publishedPresenceModes == [.manual(.away)])
        #expect(port.presenceSends.count == sends)
        await coordinator.stop()
    }

    /// The server hears the other device first and this one after, so
    /// this device's pick is the one the account ends up with.
    @Test func aPickFromAnotherDeviceHeardWhileStoringOursLosesToOurs() async {
        let (coordinator, port, _) = await connected()
        port.holdsPresenceModePublishes = true

        async let ours: Void = coordinator.setPresenceMode(.manual(.away))
        await eventually { port.heldPresenceModePublishes == 1 }
        coordinator.handle(.presenceModeChanged(.manual(.doNotDisturb)))
        port.holdsPresenceModePublishes = false
        port.releasePresenceModePublishes()
        await ours

        #expect(coordinator.status.presenceMode == .manual(.away))
        #expect(port.presenceSends.last == sent(.away))
        // Held by the server already: asking for it again stores nothing.
        await coordinator.setPresenceMode(.manual(.away))
        #expect(port.publishedPresenceModes == [.manual(.away)])
        await coordinator.stop()
    }

    /// The server did not take this device's pick, so the account holds
    /// what the other device picked.
    @Test func aPickFromAnotherDeviceStandsWhenOursIsNotTaken() async {
        let (coordinator, port, _) = await connected()
        port.holdsPresenceModePublishes = true
        port.presenceModePublishResult = .failure(.rejected)

        async let ours: Void = coordinator.setPresenceMode(.manual(.away))
        await eventually { port.heldPresenceModePublishes == 1 }
        coordinator.handle(.presenceModeChanged(.manual(.doNotDisturb)))
        port.holdsPresenceModePublishes = false
        port.releasePresenceModePublishes()
        await ours

        #expect(coordinator.status.presenceMode == .manual(.doNotDisturb))
        #expect(port.presenceSends.last == sent(.doNotDisturb))
        #expect(port.publishedPresenceModes == [.manual(.away)])
        await coordinator.stop()
    }

    @Test func aStatusPickedElsewhereAfterSigningOutIsNotTakenUp() async {
        let (coordinator, port, _) = await connected()
        await coordinator.stop()

        coordinator.handle(.presenceModeChanged(.manual(.doNotDisturb)))

        #expect(coordinator.status.presenceMode == .automatic)
        #expect(port.publishedPresenceModes.isEmpty)
    }

    // MARK: Auto-away

    @Test func tenIdleMinutesShowAwaySinceTheLastInput() async {
        let (coordinator, port, clock) = await connected()
        let lastInput = clock.now

        clock.advance(minutes: 9)
        await coordinator.refreshOwnPresence()
        #expect(port.presenceSends == [sent(.available)])

        clock.advance(minutes: 1)
        await coordinator.refreshOwnPresence()
        #expect(port.presenceSends.last == sent(.away, idleSince: lastInput))
        #expect(coordinator.status.availability == .away)

        clock.advance(minutes: 5)
        await coordinator.refreshOwnPresence()
        #expect(port.presenceSends.count == 2)

        clock.advance(minutes: 15)
        await coordinator.refreshOwnPresence()
        #expect(port.presenceSends.last == sent(.extendedAway, idleSince: lastInput))
        await coordinator.stop()
    }

    @Test func inputBringsAnIdleDeviceBack() async {
        let (coordinator, port, clock) = await connected()
        clock.advance(minutes: 12)
        await coordinator.refreshOwnPresence()

        clock.advance(minutes: 1)
        coordinator.noteActivity()
        await coordinator.refreshOwnPresence()

        #expect(port.presenceSends.last == sent(.available))
        #expect(coordinator.status.availability == .available)
        await coordinator.stop()
    }

    /// The idle time of a Mac is read from the system, so the last input
    /// may be reported after the fact and out of order.
    @Test func anOlderInputDoesNotMoveTheLastInputBack() async {
        let (coordinator, port, clock) = await connected()
        clock.advance(minutes: 12)
        coordinator.noteActivity()
        coordinator.noteActivity(at: clock.now.addingTimeInterval(-11 * 60))
        await coordinator.refreshOwnPresence()

        #expect(port.presenceSends == [sent(.available)])
        await coordinator.stop()
    }

    /// Typing on a hardware keyboard touches nothing the host can see.
    @Test func sendingAMessageCountsAsInput() async {
        let (coordinator, port, clock) = await connected()
        clock.advance(minutes: 12)

        await coordinator.send(Draft(text: "still here"), in: bobConversation)
        await coordinator.refreshOwnPresence()

        #expect(port.presenceSends == [sent(.available)])
        await coordinator.stop()
    }

    /// What a Mac says of its last input wobbles by a hair from one
    /// reading to the next.
    @Test func aLastInputReportedAHairLaterIsTheSameInput() async {
        let (coordinator, port, clock) = await connected()
        let lastInput = clock.now
        clock.advance(minutes: 12)
        await coordinator.refreshOwnPresence()

        coordinator.noteActivity(at: lastInput.addingTimeInterval(0.004))
        await coordinator.refreshOwnPresence()
        coordinator.noteActivity(at: lastInput.addingTimeInterval(0.9))
        await coordinator.refreshOwnPresence()

        #expect(port.presenceSends == [sent(.available), sent(.away, idleSince: lastInput)])
        await coordinator.stop()
    }

    @Test func typingCountsAsInput() async {
        let (coordinator, port, clock) = await connected()
        clock.advance(minutes: 12)

        coordinator.userTyped(in: bobConversation)
        await coordinator.refreshOwnPresence()

        #expect(port.presenceSends == [sent(.available)])
        await coordinator.stop()
    }

    @Test func aPickedStatusIsNotChangedByIdleTime() async {
        let (coordinator, port, clock) = await connected()
        await coordinator.setPresenceMode(.manual(.available))
        let sends = port.presenceSends.count

        clock.advance(minutes: 45)
        await coordinator.refreshOwnPresence()

        #expect(port.presenceSends.count == sends)
        #expect(coordinator.status.availability == .available)
        await coordinator.stop()
    }

    /// Choosing Automatic is itself input: idle time counts from then, not
    /// from before the status was picked.
    @Test func idleTimeCountsFromTheResetToAutomatic() async {
        let (coordinator, port, clock) = await connected()
        await coordinator.setPresenceMode(.manual(.doNotDisturb))
        clock.advance(minutes: 45)

        await coordinator.setPresenceMode(.automatic)
        #expect(port.presenceSends.last == sent(.available))

        let reset = clock.now
        clock.advance(minutes: 10)
        await coordinator.refreshOwnPresence()
        #expect(port.presenceSends.last == sent(.away, idleSince: reset))
        await coordinator.stop()
    }

    /// Being in a call is proof of presence: nobody is "away, in a call".
    @Test func aCallInProgressHoldsOffAutoAway() async {
        let (coordinator, port, clock) = await connected()
        coordinator.calls.transition(.active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin)))

        clock.advance(minutes: 45)
        await coordinator.refreshOwnPresence()
        #expect(port.presenceSends == [sent(.available)])

        // Idle time counts from the end of the call.
        coordinator.calls.transition(.idle)
        let ended = clock.now
        await coordinator.refreshOwnPresence()
        clock.advance(minutes: 10)
        await coordinator.refreshOwnPresence()
        #expect(port.presenceSends.last == sent(.away, idleSince: ended))
        await coordinator.stop()
    }

    /// A new stream knows nothing of the old one's presence.
    @Test func aReconnectSendsTheIdleStateAgain() async {
        let (coordinator, port, clock) = await connected()
        let lastInput = clock.now
        clock.advance(minutes: 12)
        await coordinator.refreshOwnPresence()

        port.emit(.disconnected)
        await eventually { !coordinator.isSendReady }
        port.presenceSends = []
        await connect(coordinator, port, again: true)

        #expect(port.presenceSends == [sent(.away, idleSince: lastInput)])
        await coordinator.stop()
    }

    @Test func nothingIsSentWhileOffline() async {
        let (coordinator, port, clock) = session()

        clock.advance(minutes: 12)
        await coordinator.refreshOwnPresence()

        #expect(port.presenceSends.isEmpty)
        #expect(coordinator.status.availability == .away)
    }

    // MARK: Status message

    @Test func aStatusMessageGoesOutWithThePresenceAlreadyShown() async {
        let (coordinator, port, clock) = await connected()
        let lastInput = clock.now
        clock.advance(minutes: 12)
        await coordinator.refreshOwnPresence()

        await coordinator.setStatusText("  Lunch ")
        #expect(port.presenceSends.last == sent(.away, idleSince: lastInput, status: "Lunch"))

        await coordinator.setStatusText(" ")
        #expect(port.presenceSends.last == sent(.away, idleSince: lastInput))
        #expect(port.publishedPresenceModes.isEmpty)
        await coordinator.stop()
    }
}

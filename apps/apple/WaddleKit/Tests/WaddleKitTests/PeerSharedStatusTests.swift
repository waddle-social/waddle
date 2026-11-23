import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// What someone else shares about the moment: their mood (XEP-0107),
/// activity (XEP-0108) and tune (XEP-0118).
@MainActor
@Suite("Someone else's shared status")
struct PeerSharedStatusTests {
    private let ada = bare("ada@example.org")
    private let happy = UserMood(value: "happy", text: "Shipped it")
    private let coding = UserActivity(general: "working", specific: "coding")
    private let song = UserTune(title: "Heroes", artist: "David Bowie")
    private let call = UserActivity(general: "talking", specific: "on_video_phone")

    private func session() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        return (SessionCoordinator(account: me, port: port), port)
    }

    @Test func whatSomeoneSharesIsFetchedFromTheirAccount() async {
        let (coordinator, port) = session()
        let shared = SharedStatus(mood: happy, activity: coding, tune: song)
        port.sharedStatus = .success(shared)

        #expect(await coordinator.sharedStatus(of: ada) == shared)
        #expect(port.sharedStatusLookups == [ada])
    }

    @Test func aLookupThatFailsSharesNothing() async {
        let (coordinator, port) = session()
        port.sharedStatus = .failure(.timeout)

        #expect(await coordinator.sharedStatus(of: ada) == nil)
    }

    // MARK: What is shown

    @Test func someoneOnlineInACallIsShownInTheCall() {
        let shared = SharedStatus(mood: happy, activity: call)

        #expect(shared.shown(isOnline: true) == shared)
    }

    /// A device that goes away without ending its call leaves the call's
    /// mark on the account (ADR-010). Offline, nobody is in a call.
    @Test func someoneOfflineIsNotShownInACall() {
        #expect(SharedStatus(mood: happy, activity: call).shown(isOnline: false) == SharedStatus(mood: happy))
    }

    /// An activity the person picked is theirs, online or not.
    @Test func anActivitySomeonePickedIsShownWhileTheyAreOffline() {
        let shared = SharedStatus(activity: coding)

        #expect(shared.shown(isOnline: false) == shared)
    }

    /// XEP-0118 §3.2: a tune with nothing in it says the music stopped.
    @Test func aTuneThatStoppedIsNotShown() {
        let shared = SharedStatus(mood: happy, tune: UserTune(title: "  "))

        #expect(shared.shown(isOnline: true) == SharedStatus(mood: happy))
    }

    @Test func someoneSharingNothingHasNothingToShow() {
        #expect(SharedStatus().shown(isOnline: true).isEmpty)
        #expect(SharedStatus(activity: call, tune: UserTune()).shown(isOnline: false).isEmpty)
        #expect(!SharedStatus(tune: song).shown(isOnline: false).isEmpty)
    }

    // MARK: Looking again

    /// A request that fails reads as nothing shared, so a later lookup that
    /// finds nothing at all leaves what is shown.
    @Test func aLaterLookupThatFindsNothingLeavesWhatIsShown() {
        let shown = SharedStatus(mood: happy, tune: song)

        #expect(shown.lookedUpAgain(SharedStatus()) == shown)
        #expect(shown.lookedUpAgain(SharedStatus(mood: happy)) == SharedStatus(mood: happy))
        #expect(shown.lookedUpAgain(SharedStatus(activity: coding)) == SharedStatus(activity: coding))
    }

    /// Looking again for the end of a call nothing will notify: the
    /// activity found replaces the one shown, even when it is none.
    @Test func lookingAgainForACallsEndTakesTheActivityFound() {
        let shown = SharedStatus(mood: happy, activity: call, tune: song)

        #expect(shown.callCheckedAgain(SharedStatus()) == SharedStatus(mood: happy, tune: song))
        #expect(shown.callCheckedAgain(SharedStatus(mood: happy, activity: call)) == SharedStatus(mood: happy, activity: call))
    }
}

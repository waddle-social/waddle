import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// Who is in a call, from their XEP-0108 activity notifications (ADR-010):
/// `talking` with `on_the_phone` or `on_video_phone`, shown only while
/// they are online.
@MainActor
@Suite("Who is in a call")
struct InCallMarkTests {
    private let call = UserActivity(general: "talking", specific: "on_the_phone")
    private let videoCall = UserActivity(general: "talking", specific: "on_video_phone")
    private let coding = UserActivity(general: "working", specific: "coding")

    private func connected() async -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        coordinator.handle(.connected)
        await eventually { coordinator.isSendReady }
        await coordinator.readyTask?.value
        return (coordinator, port)
    }

    private func online(_ coordinator: SessionCoordinator, device: String = "phone") {
        coordinator.handle(.presence(WirePresence(from: jid("bob@waddle.test/\(device)"), kind: .available, show: nil)))
    }

    private func offline(_ coordinator: SessionCoordinator, device: String = "phone") {
        coordinator.handle(.presence(WirePresence(from: jid("bob@waddle.test/\(device)"), kind: .unavailable)))
    }

    @Test(arguments: ["on_the_phone", "on_video_phone"])
    func someoneOnlineWhoseActivityShowsACallIsInACall(_ specific: String) async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: UserActivity(general: "talking", specific: specific)))

        #expect(coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    @Test func anyOtherActivityEndsTheCall() async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: call))
        coordinator.handle(.activityChanged(jid: bob, activity: coding))

        #expect(!coordinator.presence.isInCall(bob))
        #expect(coordinator.presence.activities[bob] == coding)
        await coordinator.stop()
    }

    /// Retracted, the node deleted, or an item the core could not read.
    @Test func noActivityEndsTheCall() async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: videoCall))
        coordinator.handle(.activityChanged(jid: bob, activity: nil))

        #expect(!coordinator.presence.isInCall(bob))
        #expect(coordinator.presence.activities[bob] == nil)
        await coordinator.stop()
    }

    /// `talking` alone, or `talking` in real life, is not a call.
    @Test func talkingWithoutAPhoneIsNotACall() async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: UserActivity(general: "talking", specific: "in_real_life")))

        #expect(!coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    @Test func someoneWhoseOnlinePresenceHasNotArrivedIsNotShownInACall() async {
        let (coordinator, _) = await connected()
        coordinator.handle(.activityChanged(jid: bob, activity: call))
        #expect(!coordinator.presence.isInCall(bob))

        online(coordinator)
        #expect(coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    /// A device that goes away without ending its call leaves the mark on
    /// the account; it must not come back when they return (ADR-010).
    @Test func goingOfflineForgetsTheCall() async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: call))
        offline(coordinator)
        online(coordinator)

        #expect(!coordinator.presence.isInCall(bob))
        #expect(coordinator.presence.activities[bob] == nil)
        await coordinator.stop()
    }

    @Test func goingOfflineKeepsAnActivityTheyPicked() async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: coding))
        offline(coordinator)

        #expect(coordinator.presence.activities[bob] == coding)
        await coordinator.stop()
    }

    @Test func oneDeviceGoingOfflineLeavesTheCallOfAnother() async {
        let (coordinator, _) = await connected()
        online(coordinator, device: "laptop")
        online(coordinator, device: "phone")
        coordinator.handle(.activityChanged(jid: bob, activity: call))
        offline(coordinator, device: "laptop")

        #expect(coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    /// A call that ended while the stream was down sends nothing on the
    /// next one, so no mark survives a reconnect.
    @Test func reconnectingForgetsEveryCall() async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: call))
        #expect(coordinator.presence.isInCall(bob))

        coordinator.handle(.disconnected)
        coordinator.handle(.connected)
        online(coordinator)

        #expect(!coordinator.presence.isInCall(bob))
        #expect(coordinator.presence.activities.isEmpty)
        await coordinator.stop()
    }

    @Test func signingOutForgetsEveryCall() async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: call))

        await coordinator.stop()

        #expect(coordinator.presence.activities.isEmpty)
    }

    // MARK: Looked up when a profile opens

    /// A call that began before this stream sent no notification on it; the
    /// lookup when their profile opens is how it is first learned.
    @Test func aLookupTeachesACallNotHeardOf() async {
        let (coordinator, port) = await connected()
        online(coordinator)
        port.sharedStatus = .success(SharedStatus(activity: call))

        _ = await coordinator.sharedStatus(of: bob)

        #expect(coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    @Test func aLookupEndsACallItNoLongerFinds() async {
        let (coordinator, port) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: call))
        port.sharedStatus = .success(SharedStatus(activity: coding))

        _ = await coordinator.sharedStatus(of: bob)

        #expect(coordinator.presence.activities[bob] == coding)
        await coordinator.stop()
    }

    /// The notification is newer than what the lookup read.
    @Test func aNotificationDuringALookupWinsOverIt() async {
        let (coordinator, port) = await connected()
        online(coordinator)
        port.sharedStatus = .success(SharedStatus(activity: call))
        port.sharedStatusFetches.isHolding = true

        let lookup = Task { await coordinator.sharedStatus(of: bob) }
        await eventually { port.sharedStatusFetches.count == 1 }
        coordinator.handle(.activityChanged(jid: bob, activity: nil))
        port.sharedStatusFetches.release()
        _ = await lookup.value

        #expect(!coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    /// Someone offline is in no call; one their node still holds was left
    /// behind and must not show when they return.
    @Test func aLookupForSomeoneOfflineTeachesNoCall() async {
        let (coordinator, port) = await connected()
        port.sharedStatus = .success(SharedStatus(activity: call))

        _ = await coordinator.sharedStatus(of: bob)
        online(coordinator)

        #expect(!coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    @Test func goingOfflineDuringALookupKeepsItsCallOut() async {
        let (coordinator, port) = await connected()
        online(coordinator)
        port.sharedStatus = .success(SharedStatus(activity: call))
        port.sharedStatusFetches.isHolding = true

        let lookup = Task { await coordinator.sharedStatus(of: bob) }
        await eventually { port.sharedStatusFetches.count == 1 }
        offline(coordinator)
        online(coordinator)
        port.sharedStatusFetches.release()
        _ = await lookup.value

        #expect(!coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    @Test func aLookupThatFailsLeavesWhatWasHeard() async {
        let (coordinator, port) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: call))
        port.sharedStatus = .failure(.timeout)

        _ = await coordinator.sharedStatus(of: bob)

        #expect(coordinator.presence.isInCall(bob))
        await coordinator.stop()
    }

    // MARK: What a profile shows

    /// Mood and tune come from the lookup; the activity follows what is
    /// heard after it.
    @Test func aProfileShowsTheActivityAsItChanges() async {
        let (coordinator, port) = await connected()
        online(coordinator)
        let happy = UserMood(value: "happy", text: nil)
        port.sharedStatus = .success(SharedStatus(mood: happy, activity: call))
        let lookedUp = await coordinator.sharedStatus(of: bob)

        coordinator.handle(.activityChanged(jid: bob, activity: nil))

        #expect(coordinator.liveSharedStatus(of: bob, lookedUp: lookedUp) == SharedStatus(mood: happy))
        await coordinator.stop()
    }

    @Test func aProfileWhoseLookupFailedStillShowsTheActivityHeard() async {
        let (coordinator, _) = await connected()
        online(coordinator)
        coordinator.handle(.activityChanged(jid: bob, activity: call))

        #expect(coordinator.liveSharedStatus(of: bob, lookedUp: nil) == SharedStatus(activity: call))
        #expect(coordinator.liveSharedStatus(of: bare("cy@waddle.test"), lookedUp: nil) == nil)
        await coordinator.stop()
    }

    /// No notification about the account itself reaches here (the core
    /// drops it), so its own profile shows what was looked up.
    @Test func theAccountsOwnProfileShowsWhatWasLookedUp() async {
        let (coordinator, port) = await connected()
        port.sharedStatus = .success(SharedStatus(activity: coding))
        let lookedUp = await coordinator.sharedStatus(of: me.jid)

        #expect(coordinator.liveSharedStatus(of: me.jid, lookedUp: lookedUp) == SharedStatus(activity: coding))
        #expect(coordinator.presence.activities[me.jid] == nil)
        await coordinator.stop()
    }
}

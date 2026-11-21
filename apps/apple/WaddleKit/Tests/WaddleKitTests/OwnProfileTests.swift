import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// What the account tells others about itself: its details (XEP-0292) and
/// what it is doing and listening to (XEP-0108, XEP-0118).
@MainActor
@Suite("Own profile")
struct OwnProfileTests {
    private let coding = UserActivity(general: "working", specific: "coding", text: "Shipping")
    private let song = UserTune(title: "Heroes", artist: "David Bowie")

    private func session() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        return (SessionCoordinator(account: me, port: port), port)
    }

    // MARK: Activity and tune

    @Test func openingTheProfileShowsWhatTheAccountAlreadyShares() async {
        let (coordinator, port) = session()
        port.sharedStatus = .success(SharedStatus(
            mood: UserMood(value: "happy", text: nil), activity: coding, tune: song
        ))

        await coordinator.loadOwnSharedStatus()

        #expect(coordinator.status.mood == UserMood(value: "happy", text: nil))
        #expect(coordinator.status.activity == coding)
        #expect(coordinator.status.tune == song)
    }

    /// A lookup that fails says nothing about what is shared.
    @Test func aFailedLookupKeepsWhatWasShown() async {
        let (coordinator, port) = session()
        coordinator.status.activity = coding
        port.sharedStatus = .failure(.timeout)

        await coordinator.loadOwnSharedStatus()

        #expect(coordinator.status.activity == coding)
    }

    /// The editors are usable while the profile is still loading. What is
    /// saved then is newer than what the load brings back.
    @Test func anActivitySavedWhileTheProfileLoadsIsNotReplacedByTheOlderOne() async throws {
        let (coordinator, port) = session()
        port.sharedStatus = .success(SharedStatus(mood: UserMood(value: "happy", text: nil)))
        port.holdsSharedStatusFetches = true
        let load = Task { await coordinator.loadOwnSharedStatus() }
        await eventually { port.heldSharedStatusFetches == 1 }

        try await coordinator.setActivity(coding)
        port.releaseSharedStatusFetches()
        await load.value

        #expect(coordinator.status.activity == coding)
        #expect(coordinator.status.mood == UserMood(value: "happy", text: nil))
    }

    @Test func anActivityIsPublishedAndShown() async throws {
        let (coordinator, port) = session()

        try await coordinator.setActivity(coding)

        #expect(port.publishedActivities == [coding])
        #expect(coordinator.status.activity == coding)
    }

    @Test func clearingTheActivityRetractsIt() async throws {
        let (coordinator, port) = session()
        try await coordinator.setActivity(coding)

        try await coordinator.setActivity(nil)

        #expect(port.publishedActivities == [coding, nil])
        #expect(coordinator.status.activity == nil)
    }

    @Test func anActivityTheServerRefusesIsNotShown() async {
        let (coordinator, port) = session()
        port.sharedStatusPublishResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) { try await coordinator.setActivity(coding) }

        #expect(coordinator.status.activity == nil)
    }

    /// ADR-010: a device in a call shows it through the account's one
    /// activity. Another device must not write over that, or clear it.
    @Test func anActivityIsNotWrittenOverACallOnAnotherDevice() async {
        let (coordinator, port) = session()
        let call = UserActivity(general: "talking", specific: "on_the_phone")
        port.ownActivity = .success(call)

        await #expect(throws: PortError.rejected) { try await coordinator.setActivity(coding) }
        await #expect(throws: PortError.rejected) { try await coordinator.setActivity(nil) }

        #expect(port.publishedActivities.isEmpty)
        #expect(coordinator.status.activity == call)
    }

    /// The profile stays open through a call on another device; when the
    /// call ends, the activity can be set again without reopening it.
    @Test func theActivityIsFreeAgainOnceTheCallOnAnotherDeviceEnds() async {
        let (coordinator, port) = session()
        coordinator.status.activity = UserActivity(general: "talking", specific: "on_the_phone")
        port.ownActivity = .failure(.timeout)
        await coordinator.refreshOwnActivity()
        #expect(coordinator.status.activity?.showsACall == true)

        port.ownActivity = .success(nil)
        await coordinator.refreshOwnActivity()

        #expect(coordinator.status.activity == nil)
    }

    /// A lookup that fails does not say the activity is free: a call on
    /// another device may be behind it.
    @Test func anActivityIsNotWrittenWhileWhatIsPublishedIsUnknown() async {
        let (coordinator, port) = session()
        port.ownActivity = .failure(.timeout)

        await #expect(throws: PortError.timeout) { try await coordinator.setActivity(coding) }
        await #expect(throws: PortError.timeout) { try await coordinator.setActivity(nil) }

        #expect(port.publishedActivities.isEmpty)
        #expect(coordinator.status.activity == nil)
    }

    /// Talking to someone in the room is an ordinary activity, not a call.
    @Test func anActivityThatIsNotACallCanBeReplaced() async throws {
        let (coordinator, port) = session()
        port.ownActivity = .success(UserActivity(general: "talking", specific: "in_real_life"))

        try await coordinator.setActivity(coding)

        #expect(port.publishedActivities == [coding])
    }

    @Test func aTuneIsPublishedWithoutStraySpaces() async throws {
        let (coordinator, port) = session()

        try await coordinator.setTune(UserTune(title: " Heroes ", artist: "David Bowie", album: "  "))

        #expect(port.publishedTunes == [song])
        #expect(coordinator.status.tune == song)
    }

    @Test func clearingTheTuneRetractsIt() async throws {
        let (coordinator, port) = session()
        try await coordinator.setTune(song)

        try await coordinator.setTune(nil)

        #expect(port.publishedTunes == [song, nil])
        #expect(coordinator.status.tune == nil)
    }

    /// XEP-0118 §3.2: a tune with nothing in it means "stopped", so it is
    /// never published as a tune.
    @Test func aTuneWithNothingInItIsNotPublished() async {
        let (coordinator, port) = session()

        await #expect(throws: PortError.invalidRequest) { try await coordinator.setTune(UserTune(title: "  ")) }

        #expect(port.publishedTunes.isEmpty)
    }

    // MARK: Details

    @Test func ownDetailsAreLoadedFromTheAccount() async throws {
        let (coordinator, port) = session()
        let stored = ProfileDetails(fullName: "Ada Lovelace", pronouns: "she/her")
        port.ownProfileDetails = .success(stored)

        let loaded = try await coordinator.loadOwnDetails()

        #expect(loaded == stored)
        #expect(coordinator.status.details == stored)
    }

    @Test func anAccountWithNoDetailsYetLoadsThemEmpty() async throws {
        let (coordinator, _) = session()

        #expect(try await coordinator.loadOwnDetails() == ProfileDetails())
    }

    /// Saving replaces every field, so details that could not be read must
    /// not come back as empty ones ready to be saved.
    @Test func detailsThatCouldNotBeLookedUpAreNotLoadedAsEmpty() async {
        let (coordinator, port) = session()
        port.ownProfileDetails = .failure(.rejected)

        await #expect(throws: PortError.rejected) { try await coordinator.loadOwnDetails() }

        #expect(coordinator.status.details == nil)
    }

    @Test func savedDetailsAreTrimmedAndShown() async throws {
        let (coordinator, port) = session()
        let saved = ProfileDetails(fullName: "Ada Lovelace", nickname: "ada", bio: "Maths.", link: "https://ada.example")

        try await coordinator.saveDetails(
            ProfileDetails(fullName: "  Ada Lovelace ", nickname: "ada", pronouns: "   ", bio: "Maths.\n", link: "ada.example")
        )

        #expect(port.publishedProfileDetails == [saved])
        #expect(coordinator.status.details == saved)
    }

    /// The picture's address in the details is written by whoever set the
    /// picture; editing the text must not drop it.
    @Test func savingKeepsThePictureAddress() async throws {
        let (coordinator, port) = session()
        port.ownProfileDetails = .success(ProfileDetails(fullName: "Ada", photoURI: "https://files.example/ada.png"))
        var details = try await coordinator.loadOwnDetails()
        details.fullName = "Ada Lovelace"

        try await coordinator.saveDetails(details)

        #expect(port.publishedProfileDetails.last?.photoURI == "https://files.example/ada.png")
    }

    @Test func detailsWithABrokenLinkAreNotPublished() async {
        let (coordinator, port) = session()

        await #expect(throws: PortError.invalidRequest) {
            try await coordinator.saveDetails(ProfileDetails(link: "not a link"))
        }

        #expect(port.publishedProfileDetails.isEmpty)
    }

    @Test func detailsTheServerRefusesAreNotShown() async {
        let (coordinator, port) = session()
        port.profileDetailsPublishResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) { try await coordinator.saveDetails(ProfileDetails(fullName: "Ada")) }

        #expect(coordinator.status.details == nil)
    }

    // MARK: Someone else's details

    @Test func someoneElsesDetailsAreFetchedFromTheirAccount() async {
        let (coordinator, port) = session()
        let ada = bare("ada@example.org")
        port.profileDetails[ada] = ProfileDetails(fullName: "Ada Lovelace")

        #expect(await coordinator.details(of: ada) == ProfileDetails(fullName: "Ada Lovelace"))
    }

    @Test func someoneWithNoDetailsHasNone() async {
        let (coordinator, port) = session()
        let ada = bare("ada@example.org")
        port.profileDetails[ada] = ProfileDetails(photoURI: "https://files.example/ada.png")

        #expect(await coordinator.details(of: ada) == nil)
        #expect(await coordinator.details(of: bare("nobody@example.org")) == nil)
    }
}

@Suite("Profile links")
struct ProfileLinkTests {
    @Test func aBareSiteNameBecomesAnHTTPSLink() {
        #expect(ProfileDetails(link: "ada.example/about").cleaned.link == "https://ada.example/about")
    }

    @Test func aLinkInDetailsMustBeAnAddress() {
        #expect(ProfileDetails(link: "https://ada.example").hasUsableLink)
        #expect(ProfileDetails(link: "").hasUsableLink)
        #expect(!ProfileDetails(link: "not a link").hasUsableLink)
    }

    /// A link another app stored, such as a mail address, is kept as it is
    /// and does not block saving the other fields.
    @Test func aLinkThatIsNotAWebLinkCanStillBeSaved() {
        let details = ProfileDetails(link: "mailto:ada@example.org")
        #expect(details.hasUsableLink)
        #expect(details.cleaned.link == "mailto:ada@example.org")
        #expect(details.webLink == nil)
    }

    @Test func aWebLinkIsOnlyOfferedForOpeningWhenItIsHTTP() {
        #expect(ProfileDetails(link: "https://ada.example").webLink == URL(string: "https://ada.example"))
        #expect(ProfileDetails(link: "javascript:alert(1)").webLink == nil)
        #expect(ProfileDetails(link: "file:///etc/passwd").webLink == nil)
    }

    /// A tune may point into a music app, so any full address will do.
    @Test func aTuneLinkNeedsAScheme() {
        #expect(UserTune(title: "Heroes", link: "spotify:track:abc").hasUsableLink)
        #expect(UserTune(title: "Heroes").hasUsableLink)
        #expect(!UserTune(title: "Heroes", link: "heroes").hasUsableLink)
    }
}

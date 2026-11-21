import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// The admin console: who may open it, and the list of everyone with an
/// account.
@MainActor
@Suite("Admin console")
struct AdminTests {
    private func connected(isOwner: Bool) async -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        port.calendar.isOwner = isOwner
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        port.emit(.connected)
        await eventually { coordinator.isSendReady }
        await coordinator.readyTask?.value
        return (coordinator, port)
    }

    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        return (coordinator, port)
    }

    private let ada = AdminUser(jid: bare("ada@waddle.test"), displayName: "Ada Lovelace", isOwner: true)
    private let bo = AdminUser(jid: bare("bo@waddle.test"), displayName: nil, isOwner: false)
    private let cy = AdminUser(jid: bare("cy@waddle.test"), displayName: "Cy", isOwner: false)

    // MARK: Who may open it

    @Test func aCommunityOwnerIsKnownOnceConnected() async {
        let (coordinator, _) = await connected(isOwner: true)

        #expect(coordinator.isCommunityOwner)
        await coordinator.stop()
    }

    @Test func aMemberIsNotACommunityOwner() async {
        let (coordinator, _) = await connected(isOwner: false)

        #expect(!coordinator.isCommunityOwner)
        await coordinator.stop()
    }

    /// The question answers "no" when it cannot be asked, so a "no" after
    /// a "yes" is not taken as news.
    @Test func anOwnerStaysOneWhenALaterAnswerSaysNo() async {
        let (coordinator, port) = await connected(isOwner: true)
        port.calendar.isOwner = false

        await coordinator.refreshCommunityOwnership()

        #expect(coordinator.isCommunityOwner)
        await coordinator.stop()
    }

    /// The first question can time out, which reads as "no". Events asks
    /// the same thing on every refresh.
    @Test func anOwnerMissedOnConnectingIsFoundWhenEventsRefresh() async {
        let (coordinator, port) = await connected(isOwner: false)
        port.calendar.isOwner = true

        await coordinator.refreshEvents()

        #expect(coordinator.isCommunityOwner)
        await coordinator.stop()
    }

    @Test func signingOutForgetsTheOwner() async {
        let (coordinator, _) = await connected(isOwner: true)

        await coordinator.stop()

        #expect(!coordinator.isCommunityOwner)
    }

    /// An answer that arrives after signing out belongs to nobody.
    @Test func anAnswerAfterSigningOutIsDropped() async {
        let (coordinator, port) = await connected(isOwner: false)
        port.calendar.isOwner = true
        port.calendar.held = ["isCommunityOwner"]
        let asking = Task { await coordinator.refreshCommunityOwnership() }
        await eventually { port.calendar.ownerRequests == 2 }

        await coordinator.stop()
        port.calendar.release()
        await asking.value

        #expect(!coordinator.isCommunityOwner)
    }

    // MARK: Users

    @Test func aUserIsNamedByTheirDisplayNameOrTheirAddress() {
        #expect(ada.name == "Ada Lovelace")
        #expect(bo.name == "bo")
        #expect(AdminUser(jid: bare("dee@waddle.test"), displayName: "  ", isOwner: false).name == "dee")
    }

    @Test func theUsersListLoadsItsFirstPage() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada, bo], nextCursor: nil))
        let list = coordinator.adminUsersList()

        await list.load()

        #expect(list.rows == [ada, bo])
        #expect(list.hasLoaded)
        #expect(!list.hasMore)
        #expect(!list.isLoading)
        #expect(list.failure == nil)
    }

    @Test func loadingMoreAddsTheNextPage() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada], nextCursor: "after-ada"))
        port.admin.users[.init(cursor: "after-ada")] = .success(AdminPage(rows: [bo], nextCursor: nil))
        let list = coordinator.adminUsersList()
        await list.load()
        #expect(list.hasMore)

        await list.loadMore()

        #expect(list.rows == [ada, bo])
        #expect(!list.hasMore)
    }

    /// A list that changed between two pages can name someone twice.
    @Test func someoneOnTwoPagesIsListedOnce() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada, bo], nextCursor: "next"))
        port.admin.users[.init(cursor: "next")] = .success(AdminPage(rows: [bo, cy], nextCursor: nil))
        let list = coordinator.adminUsersList()
        await list.load()

        await list.loadMore()

        #expect(list.rows == [ada, bo, cy])
    }

    @Test func searchingAsksForNamesThatStartWithTheText() async {
        let (coordinator, port) = online()
        port.admin.users[.init(prefix: "a")] = .success(AdminPage(rows: [ada], nextCursor: "more-a"))
        port.admin.users[.init(prefix: "a", cursor: "more-a")] = .success(AdminPage(rows: [cy], nextCursor: nil))
        let list = coordinator.adminUsersList()

        await list.load(matching: " a ")
        await list.loadMore()

        #expect(list.rows == [ada, cy])
        #expect(port.admin.usersRequests == [.init(prefix: "a"), .init(prefix: "a", cursor: "more-a")])
    }

    @Test func aSlowerAnswerToEarlierTextDoesNotReplaceANewerOne() async {
        let (coordinator, port) = online()
        port.admin.users[.init(prefix: "a")] = .success(AdminPage(rows: [ada], nextCursor: nil))
        port.admin.users[.init(prefix: "b")] = .success(AdminPage(rows: [bo], nextCursor: nil))
        let list = coordinator.adminUsersList()
        port.admin.usersFetches.isHolding = true
        let first = Task { await list.load(matching: "a") }
        await eventually { port.admin.usersFetches.count == 1 }
        port.admin.usersFetches.isHolding = false

        await list.load(matching: "b")
        port.admin.usersFetches.release()
        await first.value

        #expect(list.rows == [bo])
        #expect(!list.isLoading)
    }

    @Test func aNewSearchDropsTheNextPageOfTheOldOne() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada], nextCursor: "next"))
        port.admin.users[.init(cursor: "next")] = .success(AdminPage(rows: [cy], nextCursor: nil))
        port.admin.users[.init(prefix: "b")] = .success(AdminPage(rows: [bo], nextCursor: nil))
        let list = coordinator.adminUsersList()
        await list.load()
        port.admin.usersFetches.isHolding = true
        let more = Task { await list.loadMore() }
        await eventually { port.admin.usersFetches.count == 1 }
        port.admin.usersFetches.isHolding = false

        await list.load(matching: "b")
        port.admin.usersFetches.release()
        await more.value

        #expect(list.rows == [bo])
        #expect(!list.isLoadingMore)
    }

    /// Typing on cancels the search in flight; that is not a failure.
    @Test func aCancelledSearchChangesNothingShown() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada], nextCursor: nil))
        port.admin.users[.init(prefix: "b")] = .failure(.failed)
        let list = coordinator.adminUsersList()
        await list.load()
        port.admin.usersFetches.isHolding = true
        let search = Task { await list.load(matching: "b") }
        await eventually { port.admin.usersFetches.count == 1 }

        search.cancel()
        port.admin.usersFetches.release()
        await search.value

        #expect(list.rows == [ada])
        #expect(list.query == "")
        #expect(list.failure == nil)
        #expect(!list.isLoading)
    }

    /// What is said about an empty list is said about the text answered.
    @Test func theListNamesTheTextItAnswered() async {
        let (coordinator, port) = online()
        let list = coordinator.adminUsersList()
        await list.load(matching: "zz")
        #expect(list.query == "zz")
        port.admin.usersFetches.isHolding = true
        let cleared = Task { await list.load() }
        await eventually { port.admin.usersFetches.count == 1 }

        #expect(list.query == "zz")

        port.admin.usersFetches.release()
        await cleared.value
        #expect(list.query == "")
    }

    /// The text cleared before a search answers: everyone stays shown, and
    /// a refresh asks for everyone, not the text.
    @Test func aRefreshAfterACancelledSearchAsksForWhatIsShown() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada, bo], nextCursor: nil))
        let list = coordinator.adminUsersList()
        await list.load()
        port.admin.usersFetches.isHolding = true
        let search = Task { await list.load(matching: "b") }
        await eventually { port.admin.usersFetches.count == 1 }

        search.cancel()
        port.admin.usersFetches.isHolding = false
        port.admin.usersFetches.release()
        await search.value
        await list.reload()

        #expect(port.admin.usersRequests.last == .init())
        #expect(list.rows == [ada, bo])
        #expect(list.query == "")
    }

    /// A search that overtakes the first page leaves no page to ask for
    /// yet; loading every page stops instead of asking forever.
    @Test(.timeLimit(.minutes(1)))
    func loadingEveryPageStopsWhenAnotherSearchOvertakesIt() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada], nextCursor: "next"))
        port.admin.users[.init(cursor: "next")] = .success(AdminPage(rows: [bo], nextCursor: nil))
        port.admin.users[.init(prefix: "c")] = .success(AdminPage(rows: [cy], nextCursor: nil))
        let list = coordinator.adminUsersList()
        await list.load()
        port.admin.usersFetches.isHolding = true
        let every = Task { await list.loadEveryPage() }
        await eventually { port.admin.usersFetches.count == 1 }
        let search = Task { await list.load(matching: "c") }
        await eventually { port.admin.usersFetches.count == 2 }

        port.admin.usersFetches.isHolding = false
        port.admin.usersFetches.release()
        await every.value
        await search.value

        #expect(list.rows == [cy])
    }

    @Test func refreshingDuringASearchAsksForTheTextBeingSearched() async {
        let (coordinator, port) = online()
        port.admin.users[.init(prefix: "a")] = .success(AdminPage(rows: [ada], nextCursor: nil))
        port.admin.users[.init(prefix: "b")] = .success(AdminPage(rows: [bo], nextCursor: nil))
        let list = coordinator.adminUsersList()
        await list.load(matching: "a")
        port.admin.usersFetches.isHolding = true
        let search = Task { await list.load(matching: "b") }
        await eventually { port.admin.usersFetches.count == 1 }
        port.admin.usersFetches.isHolding = false

        await list.reload()
        port.admin.usersFetches.release()
        await search.value

        #expect(list.rows == [bo])
        #expect(list.query == "b")
    }

    @Test func someoneWhoIsNotAnOwnerIsRefused() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .failure(.rejected)
        let list = coordinator.adminUsersList()

        await list.load()

        #expect(list.rows.isEmpty)
        #expect(list.failure == .rejected)
        #expect(!list.hasLoaded)
    }

    @Test func aRefreshThatFailsKeepsTheUsersShown() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada], nextCursor: "next"))
        let list = coordinator.adminUsersList()
        await list.load()
        port.admin.users[.init()] = .failure(.timeout)

        await list.reload()

        #expect(list.rows == [ada])
        #expect(list.hasMore)
        #expect(list.failure == .timeout)
    }

    /// Users found for other text are not an answer to this text.
    @Test func aSearchThatFailsShowsNoOneFromTheSearchBefore() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada], nextCursor: "next"))
        port.admin.users[.init(prefix: "b")] = .failure(.timeout)
        let list = coordinator.adminUsersList()
        await list.load()

        await list.load(matching: "b")

        #expect(list.rows.isEmpty)
        #expect(!list.hasMore)
        #expect(!list.hasLoaded)
        #expect(list.failure == .timeout)
    }

    @Test func tryingAgainAfterAFailureLoadsTheUsers() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .failure(.timeout)
        let list = coordinator.adminUsersList()
        await list.load()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada], nextCursor: nil))

        await list.reload()

        #expect(list.rows == [ada])
        #expect(list.failure == nil)
    }

    @Test func aNextPageThatFailsCanBeAskedForAgain() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(rows: [ada], nextCursor: "next"))
        port.admin.users[.init(cursor: "next")] = .failure(.timeout)
        let list = coordinator.adminUsersList()
        await list.load()

        await list.loadMore()

        #expect(list.rows == [ada])
        #expect(list.hasMore)
        #expect(list.moreFailure == .timeout)
        #expect(list.failure == nil)

        port.admin.users[.init(cursor: "next")] = .success(AdminPage(rows: [bo], nextCursor: nil))
        await list.loadMore()

        #expect(list.rows == [ada, bo])
        #expect(list.moreFailure == nil)
    }

    @Test func nothingIsAskedWhileOffline() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        let list = coordinator.adminUsersList()

        await list.load()

        #expect(list.failure == .notConnected)
        #expect(port.admin.usersRequests.isEmpty)
    }
}

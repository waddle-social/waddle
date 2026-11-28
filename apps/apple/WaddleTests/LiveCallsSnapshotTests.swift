import SwiftUI
import WaddleKit
import XCTest

@testable import Waddle

/// The sidebar's "Live now" cards, on the rail they sit in.
@MainActor
final class LiveCallsSnapshotTests: XCTestCase {
    /// The sidebar's widths (`SplitShell`): the one it opens at and the narrowest it can be dragged to.
    private static let idealSidebar: CGFloat = 280
    private static let narrowestSidebar: CGFloat = 240

    private func people(_ names: String...) -> [GroupCallRosterRow] {
        names.map { GroupCallRosterRow(nick: $0, jid: BareJID(parsing: "\($0.lowercased())@waddle.test")) }
    }

    private func call(_ title: String, _ people: [GroupCallRosterRow], video: Bool = false, action: LiveCall.Action = .join) -> LiveCall {
        LiveCall(
            room: BareJID(parsing: "\(title.lowercased().replacingOccurrences(of: " ", with: "-"))@rooms.waddle.test")!,
            title: title, people: people, hasVideo: video, action: action
        )
    }

    /// The list as `LiveCallsHost` places it in a sidebar `sidebar` wide.
    private func assertList(_ calls: [LiveCall], sidebar: CGFloat = idealSidebar, testName: String = #function, line: UInt = #line) {
        assertHuddleSnapshot(
            of: LiveCallsList(calls: calls) { _ in /* nothing to join */ }
                .padding(.horizontal, Theme.Spacing.m)
                .padding(.vertical, Theme.Spacing.s)
                .frame(width: sidebar)
                .background(Theme.Colors.bgRail)
                .environment(\.huddleGround, Theme.Colors.bgRail),
            width: sidebar + 2 * Theme.Spacing.l,
            testName: testName,
            line: line
        )
    }

    func testTwoRoomsWithCalls() {
        assertList([
            call("design", people("Ada", "Linus", "Grace", "Margaret")),
            call("standup", people("Priya"), video: true),
        ])
    }

    /// In another call already: the card opens the room and does not join.
    func testACallThatCanOnlyBeOpened() {
        assertList([call("design", people("Ada", "Linus"), action: .open)])
    }

    /// A call of two that our phone answered: this device can open the conversation, not the call.
    func testACallOnAnotherOfOurDevicesBesideARoomsCall() {
        let ada = CallElsewhere(sid: "s1", peer: BareJID(parsing: "ada@waddle.test")!, media: CallMedia(audio: true, video: true), since: Date())
        assertList([
            call("design", people("Linus", "Grace")),
            LiveCall(elsewhere: ada, name: "Ada Lovelace"),
        ])
    }

    func testALongRoomNameInTheNarrowestSidebar() {
        assertList(
            [
                call("quarterly planning and retrospective", people("Ada", "Linus", "Grace"), video: true),
                call("design", people("Priya"), action: .open),
            ],
            sidebar: Self.narrowestSidebar
        )
    }
}

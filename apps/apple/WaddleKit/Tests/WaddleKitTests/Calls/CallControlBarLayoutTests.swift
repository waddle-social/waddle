import Testing

@testable import WaddleKit

/// Where each control of a call's bar is, layout by layout. The More
/// menu's contents are in no golden: this table is what pins them.
struct CallControlBarLayoutTests {
    private typealias Layout = CallControlBarLayout
    private typealias Place = CallControlBarLayout.Place

    /// A room's call on a Mac with everything there is, nothing in progress.
    private let room = CallControls(
        devices: .init(isSharingScreen: false), isHandRaised: false, chatUnread: 0, canReact: true
    )

    private func places(_ control: Layout.Control, _ controls: CallControls) -> [Place] {
        Layout.allCases.map { $0.place(of: control, for: controls) }
    }

    @Test func theLayoutsAreTriedWidestFirst() {
        #expect(Layout.allCases == [.oneRow, .twoRows, .viewChoiceInMenu, .reactionsInMenu, .extrasInMenu])
        #expect(Layout.allCases.map(\.stacksEndButton) == [false, true, true, true, true])
    }

    @Test func theViewChoiceIsTheFirstToGo() {
        #expect(places(.viewChoice, room) == [.bar, .bar, .menu, .menu, .menu])
    }

    @Test func reactingGoesBeforeSharingAndTheListOfPeople() {
        #expect(places(.react, room) == [.bar, .bar, .bar, .menu, .menu])
        #expect(places(.screenShare, room) == [.bar, .bar, .bar, .bar, .menu])
        #expect(places(.participants, room) == [.bar, .bar, .bar, .bar, .menu])
        #expect(places(.chat, room) == [.bar, .bar, .bar, .bar, .menu])
    }

    @Test func aHandIsNeverInTheMenu() {
        #expect(places(.hand, room) == Array(repeating: .bar, count: 5))
    }

    @Test func aShareInProgressKeepsItsButton() {
        var sharing = room
        sharing.devices.isSharingScreen = true
        #expect(places(.screenShare, sharing) == Array(repeating: .bar, count: 5))
    }

    @Test func aChatWithSomethingUnreadOrOpenKeepsItsButton() {
        var unread = room
        unread.chatUnread = 3
        #expect(places(.chat, unread) == Array(repeating: .bar, count: 5))
        var open = room
        open.showsChat = true
        #expect(places(.chat, open) == Array(repeating: .bar, count: 5))
    }

    /// A call of two on an iPad, still connecting: no hand, no share, no chat, nothing to react in.
    @Test func whatACallDoesNotHaveIsNowhere() {
        let bare = CallControls()
        for control in [Layout.Control.hand, .screenShare, .chat, .react] {
            #expect(places(control, bare) == Array(repeating: .absent, count: 5))
        }
        #expect(places(.participants, bare) == [.bar, .bar, .bar, .bar, .menu])
    }

}

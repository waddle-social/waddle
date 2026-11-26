/// The ways a call's control bar is laid out, widest first. The bar takes
/// the first that fits its width: as it narrows, controls move one group
/// at a time out of the row and into its More menu.
public enum CallControlBarLayout: CaseIterable, Sendable {
    /// Everything on one row, the pill that ends the call at the end of it.
    case oneRow
    /// The pill takes a row of its own under the others.
    case twoRows
    /// The 400 pt column of a room's call: the view choice moves into More.
    case viewChoiceInMenu
    /// One button too many for that column: reacting goes into More before sharing and the list of people do.
    case reactionsInMenu
    /// Narrower still, or with every button there is: sharing, the chat and the list of people go there too.
    case extrasInMenu

    /// The controls whose place depends on the layout or on the call. The
    /// microphone, the camera, the size steps and More itself are always
    /// in the row; hiding our own view and the call's settings are always in More.
    public enum Control: CaseIterable, Sendable {
        case viewChoice, hand, screenShare, chat, react, participants, pictureInPicture
    }

    public enum Place: Equatable, Sendable {
        case bar, menu
        /// The call has no such control: a call of two has no hand, an iPad cannot share its screen.
        case absent
    }

    /// The pill that ends the call has a row to itself.
    public var stacksEndButton: Bool { self != .oneRow }

    /// Where `control` is in this layout, for a call whose controls are `controls`.
    ///
    /// Three things keep their button whatever the width, because they must
    /// be one click away: a share in progress, to stop it, a chat with
    /// something unread in it or open, since a count nobody can see is no
    /// count and an open chat must be one click from closed, and a floating
    /// call, to bring it back.
    public func place(of control: Control, for controls: CallControls) -> Place {
        let extrasInMenu = self == .extrasInMenu
        switch control {
        case .viewChoice:
            switch self {
            case .oneRow, .twoRows: return .bar
            case .viewChoiceInMenu, .reactionsInMenu, .extrasInMenu: return .menu
            }
        case .hand:
            return controls.isHandRaised == nil ? .absent : .bar
        case .screenShare:
            guard let isSharing = controls.devices.isSharingScreen else { return .absent }
            return !extrasInMenu || isSharing ? .bar : .menu
        case .chat:
            guard let unread = controls.chatUnread else { return .absent }
            return !extrasInMenu || unread > 0 || controls.showsChat ? .bar : .menu
        case .react:
            guard controls.canReact else { return .absent }
            return self == .reactionsInMenu || extrasInMenu ? .menu : .bar
        case .participants:
            return extrasInMenu ? .menu : .bar
        case .pictureInPicture:
            guard let isFloating = controls.isFloating else { return .absent }
            return !extrasInMenu || isFloating ? .bar : .menu
        }
    }
}

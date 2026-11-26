/// What a call's controls show: the state of our own devices and of the
/// stage's own choices, gathered once by whoever draws the call and handed on whole.
public struct CallControls: Equatable, Sendable {
    /// Our devices, as the controls and the sidebar's card both show them.
    public struct Devices: Equatable, Sendable {
        public var isMicrophoneOn: Bool
        public var isCameraOn: Bool
        /// Media has connected: the devices can be switched.
        public var hasMedia: Bool
        /// Whether we are sharing our screen; `nil` where this device cannot.
        public var isSharingScreen: Bool?

        public init(
            isMicrophoneOn: Bool = false, isCameraOn: Bool = false, hasMedia: Bool = true, isSharingScreen: Bool? = nil
        ) {
            self.isMicrophoneOn = isMicrophoneOn
            self.isCameraOn = isCameraOn
            self.hasMedia = hasMedia
            self.isSharingScreen = isSharingScreen
        }
    }

    public var devices: Devices
    /// Whether our hand is up; `nil` in a call with no hands to raise, a 1:1.
    public var isHandRaised: Bool?
    public var viewMode: CallStagePresentation.ViewMode
    public var isSelfViewHidden: Bool
    public var size: CallSurface.Size
    /// Off while something else on screen has a use for Escape.
    public var escapeStepsDown: Bool
    public var showsParticipants: Bool
    /// The messages of the call's chat not yet seen; `nil` for a call with no chat to offer.
    public var chatUnread: Int?
    public var showsChat: Bool
    /// Media has connected: there is a call to react in. Kept apart from `devices.hasMedia`:
    /// a caller may show a bar with one and not the other.
    public var canReact: Bool
    /// Whether the call floats above the other windows; `nil` where it cannot,
    /// or while nobody has a picture to float.
    public var isFloating: Bool?
    /// The stage covers a phone's screen: in place of the steps in size it has one way back to the dock.
    public var minimises: Bool

    public init(
        devices: Devices = Devices(), isHandRaised: Bool? = nil, viewMode: CallStagePresentation.ViewMode = .gallery,
        isSelfViewHidden: Bool = false, size: CallSurface.Size = .split, escapeStepsDown: Bool = true,
        showsParticipants: Bool = false, chatUnread: Int? = nil, showsChat: Bool = false, canReact: Bool = false,
        isFloating: Bool? = nil, minimises: Bool = false
    ) {
        self.devices = devices
        self.isHandRaised = isHandRaised
        self.viewMode = viewMode
        self.isSelfViewHidden = isSelfViewHidden
        self.size = size
        self.escapeStepsDown = escapeStepsDown
        self.showsParticipants = showsParticipants
        self.chatUnread = chatUnread
        self.showsChat = showsChat
        self.canReact = canReact
        self.isFloating = isFloating
        self.minimises = minimises
    }
}

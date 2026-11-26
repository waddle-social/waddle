/// Which conversation actions the toolbar shows and which it folds into a
/// More menu. On an iPhone the bar also carries the back button and the
/// title, so it keeps three trailing items, as Android's conversation bar
/// does, and the conversation's name stays readable.
struct ConversationToolbarLayout: Equatable {
    enum Action: Hashable {
        /// One menu with a voice and a video call.
        case call
        case voiceCall
        case videoCall
        case search
        case pins
        case details
    }

    let inline: [Action]
    let more: [Action]

    /// `isCompact`: details push onto a phone stack rather than open in
    /// the inspector.
    init(isRoom: Bool, isCompact: Bool) {
        switch (isRoom, isCompact) {
        case (true, true):
            inline = [.call, .search]
            more = [.pins, .details]
        case (false, true):
            inline = [.call, .search]
            more = [.pins, .details]
        case (true, false):
            inline = [.call, .search, .pins, .details]
            more = []
        case (false, false):
            inline = [.voiceCall, .videoCall, .search, .pins, .details]
            more = []
        }
    }
}

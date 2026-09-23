import SwiftUI
import WaddleKit

/// Details for a room or a DM. Pushed on iPhone, shown in the inspector on
/// iPad and Mac.
struct ConversationDetailsView: View {
    let conversation: ConversationID

    init(conversation: ConversationID) {
        self.conversation = conversation
    }

    var body: some View {
        Group {
            switch conversation.kind {
            case .room:
                // Keyed so a different room never reuses another room's
                // member state.
                RoomDetailsForm(room: conversation.jid)
                    .id(conversation)
            case .direct:
                DirectDetailsForm(peer: conversation.jid)
            }
        }
        .navigationTitle("Details")
        .detailsInlineTitle()
    }
}

private extension View {
    func detailsInlineTitle() -> some View {
        #if os(iOS)
        return self.navigationBarTitleDisplayMode(.inline)
        #else
        return self
        #endif
    }
}

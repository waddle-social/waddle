import SwiftUI

/// Toolbar button that starts a new message.
struct NewMessageToolbarButton: View {
    @Environment(NavigationModel.self) private var navigation

    var body: some View {
        Button {
            navigation.sheet = .newMessage
        } label: {
            Label("New message", systemImage: "square.and.pencil")
        }
        .help("New message")
    }
}

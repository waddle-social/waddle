import SwiftUI
import WaddleKit

/// Conversation toolbar: the title (iPhone and iPad), calls for a 1:1,
/// search, pins for rooms, and details.
struct ConversationToolbar: ToolbarContent {
    let conversation: ConversationID
    /// No other call is in the slot and the session is online.
    let canCall: Bool
    /// Starts a call; `true` for video.
    let onCall: (Bool) -> Void
    let onSearch: () -> Void
    let onPins: () -> Void
    let onDetails: () -> Void

    var body: some ToolbarContent {
        #if os(iOS)
        ToolbarItem(placement: .principal) {
            ConversationTitleView(conversation: conversation)
        }
        #endif
        ToolbarItemGroup(placement: .primaryAction) {
            if !conversation.isRoom {
                Button { onCall(false) } label: {
                    Label("Voice call", systemImage: "phone")
                }
                .help("Start a voice call")
                .disabled(!canCall)
                Button { onCall(true) } label: {
                    Label("Video call", systemImage: "video")
                }
                .help("Start a video call")
                .disabled(!canCall)
            }
            Button(action: onSearch) {
                Label("Search", systemImage: "magnifyingglass")
            }
            .help("Search in conversation")
            if conversation.isRoom {
                Button(action: onPins) {
                    Label("Pinned messages", systemImage: "pin")
                }
                .help("Pinned messages")
            }
            Button(action: onDetails) {
                Label("Details", systemImage: "info.circle")
            }
            .help("Conversation details")
        }
    }
}

/// Two-line title: name with `#` or presence, and a member count or
/// availability underneath.
struct ConversationTitleView: View {
    @Environment(SessionCoordinator.self) private var session
    let conversation: ConversationID

    var body: some View {
        let header = ConversationHeaderText.make(for: conversation, session: session)
        VStack(spacing: 0) {
            HStack(spacing: Theme.Spacing.xs) {
                leadingMark(isChannel: header.isChannel)
                Text(header.name)
                    .font(.headline)
                    .lineLimit(1)
            }
            if let subtitle = ConversationHeaderText.subtitle(for: conversation, session: session) {
                Text(subtitle)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }

    @ViewBuilder
    private func leadingMark(isChannel: Bool) -> some View {
        if !conversation.isRoom {
            PresenceDot(availability: session.presence.availability(of: conversation.jid), size: 8)
        } else if isChannel {
            Image(systemName: "number")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
        } else {
            Image(systemName: "person.2.fill")
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
        }
    }
}

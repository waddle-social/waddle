import SwiftUI
import WaddleKit

/// Conversation toolbar: the title (iPhone and iPad), calls, search, pins
/// for rooms, and details. On an iPhone, a room's pins and details fold
/// into a More menu (see `ConversationToolbarLayout`).
struct ConversationToolbar: ToolbarContent {
    let conversation: ConversationID
    /// Details push onto a phone stack rather than open in the inspector.
    let isCompact: Bool
    /// No other call is in the slot and the session is online.
    let canCall: Bool
    /// The room has a call in progress, so the call buttons join it.
    var roomHasCall = false
    /// Starts a call, or joins the room's; `true` for video.
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
        let layout = ConversationToolbarLayout(isRoom: conversation.isRoom, isCompact: isCompact)
        ToolbarItemGroup(placement: .primaryAction) {
            ForEach(layout.inline, id: \.self) { action in
                control(for: action)
            }
            if !layout.more.isEmpty {
                Menu {
                    ForEach(layout.more, id: \.self) { action in
                        control(for: action)
                    }
                } label: {
                    Label("More", systemImage: "ellipsis")
                }
                .help("More actions")
            }
        }
    }

    @ViewBuilder
    private func control(for action: ConversationToolbarLayout.Action) -> some View {
        switch action {
        case .call:
            // One item for voice and video, so the bar leaves the title room.
            Menu {
                Button { onCall(false) } label: {
                    Label(roomHasCall ? "Join with voice" : "Start a voice call", systemImage: "phone")
                }
                Button { onCall(true) } label: {
                    Label(roomHasCall ? "Join with video" : "Start a video call", systemImage: "video")
                }
            } label: {
                Label(roomHasCall ? "Join call" : "Start call", systemImage: roomHasCall ? "phone.fill" : "phone")
            }
            .help(roomHasCall ? "Join the call in this room" : conversation.isRoom ? "Start a call in this room" : "Start a call")
            .disabled(!canCall)
        case .voiceCall:
            Button { onCall(false) } label: {
                Label("Voice call", systemImage: "phone")
            }
            .help("Start a voice call")
            .disabled(!canCall)
        case .videoCall:
            Button { onCall(true) } label: {
                Label("Video call", systemImage: "video")
            }
            .help("Start a video call")
            .disabled(!canCall)
        case .search:
            Button(action: onSearch) {
                Label("Search", systemImage: "magnifyingglass")
            }
            .help("Search in conversation")
        case .pins:
            Button(action: onPins) {
                Label("Pinned messages", systemImage: "pin")
            }
            .help("Pinned messages")
        case .details:
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
                    .huddleText(Theme.Typography.channel)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
            }
            if let subtitle = ConversationHeaderText.subtitle(for: conversation, session: session) {
                Text(subtitle)
                    .huddleText(Theme.Typography.caption.sized(WaddleTokens.FontSize.xxs))
                    .foregroundStyle(Theme.Colors.fgMuted)
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
                .huddleText(Theme.Typography.label)
                .foregroundStyle(Theme.Colors.fgSoft)
                .accessibilityHidden(true)
        } else {
            Image(systemName: "person.2.fill")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
                .accessibilityHidden(true)
        }
    }
}

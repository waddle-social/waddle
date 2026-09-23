import SwiftUI
import WaddleKit

/// Picks between the first-load skeleton, a failed first load, the empty
/// conversation state and the message list.
struct ConversationTimelineContainer: View {
    @Environment(SessionCoordinator.self) private var session
    let conversation: ConversationID
    let header: ConversationHeaderText
    let unreadAnchorID: String?

    var body: some View {
        let timeline = session.timelines.timeline(for: conversation)
        let history = session.history.state(of: conversation)
        if !timeline.items.contains(where: \.isFeedVisible) {
            if history.hasLoadedLatest {
                ConversationEmptyState(conversation: conversation, header: header)
            } else if history.failed, !history.isLoading {
                TimelineLoadFailed {
                    Task { await session.loadLatest(conversation) }
                }
            } else {
                TimelineSkeleton()
            }
        } else {
            ConversationTimelineList(
                conversation: conversation,
                header: header,
                unreadAnchorID: unreadAnchorID
            )
        }
    }
}

/// The newest page could not be fetched and nothing is cached to show.
struct TimelineLoadFailed: View {
    let retry: () -> Void

    var body: some View {
        ContentUnavailableView {
            Label("Couldn't Load Messages", systemImage: "exclamationmark.bubble")
        } description: {
            Text("Check your connection and try again.")
        } actions: {
            Button("Try Again", action: retry)
                .buttonStyle(.borderedProminent)
        }
    }
}

/// Friendly first-message prompt.
struct ConversationEmptyState: View {
    let conversation: ConversationID
    let header: ConversationHeaderText

    var body: some View {
        VStack(spacing: Theme.Spacing.m) {
            mark
            Text(header.emptyTitle)
                .font(.title3.weight(.semibold))
            Text(header.emptyMessage)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .multilineTextAlignment(.center)
        .padding(Theme.Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var mark: some View {
        if conversation.isRoom {
            Image(systemName: header.isChannel ? "number" : "person.2.fill")
                .font(.system(size: 28, weight: .semibold))
                .foregroundStyle(Color.accentColor)
                .frame(width: 64, height: 64)
                .background(Circle().fill(Color.accentColor.opacity(0.12)))
                .accessibilityHidden(true)
        } else {
            JIDAvatar(jid: conversation.jid, size: 64)
        }
    }
}

/// Placeholder rows while the newest page loads.
struct TimelineSkeleton: View {
    private let lineWidths: [CGFloat] = [180, 240, 120, 210, 160, 230]

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.l + 2) {
            Spacer(minLength: 0)
            ForEach(lineWidths.indices, id: \.self) { index in
                HStack(alignment: .top, spacing: Theme.Spacing.s + 2) {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .frame(width: Theme.Size.avatar, height: Theme.Size.avatar)
                    VStack(alignment: .leading, spacing: 6) {
                        RoundedRectangle(cornerRadius: 4).frame(width: 90, height: 10)
                        RoundedRectangle(cornerRadius: 4).frame(width: lineWidths[index], height: 10)
                    }
                }
            }
        }
        .foregroundStyle(Color.secondary.opacity(0.15))
        .padding(Theme.Spacing.l)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("Loading messages"))
    }
}

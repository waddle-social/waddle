import SwiftUI
#if os(iOS)
import UIKit
#endif

struct ChatTimelineView: View {
    let messages: [ChatTimelineMessage]
    var historyState: ChatRoomHistoryState = .init()
    var onLoadOlderMessages: (() -> Void)? = nil
    var onReply: ((ChatTimelineMessage) -> Void)? = nil
    var onRetract: ((ChatTimelineMessage) -> Void)? = nil
    var onOpenThread: ((ChatTimelineMessage) -> Void)? = nil
    /// Parent-message-id → ordered list of thread-child message ids. The
    /// timeline uses this to render the "💬 N replies" chip on root messages.
    var childrenByThreadID: [String: [String]] = [:]
    /// Id of the first message to flag as unread. When non-nil the timeline
    /// inserts a red-accented "New" divider directly above this message.
    var firstUnreadMessageID: String? = nil
    /// XEP-0084 avatar bytes keyed by message `senderID`. Rows call
    /// `onRequestAvatar` on first appear; we render whatever data is
    /// currently in this map and fall back to initials otherwise.
    var avatarDataBySenderID: (String) -> Data? = { _ in nil }
    var onRequestAvatar: ((String) -> Void)? = nil
    var emptyState: AnyView? = nil
    var usesOperationalDensity: Bool = false
    var usesCompactConversationStyle: Bool = false
    @AppStorage(AppConfig.scrollDirectionKey) private var scrollDirectionRaw = ChatScrollDirection.chat.rawValue

    private var scrollDirection: ChatScrollDirection {
        ChatScrollDirection(rawValue: scrollDirectionRaw) ?? .chat
    }

    private var displayedMessages: [ChatTimelineMessage] {
        switch scrollDirection {
        case .chat:
            return messages
        case .social:
            return Array(messages.reversed())
        }
    }

    private var stackSpacing: CGFloat {
        if usesCompactConversationStyle {
            return 0
        }
        return usesOperationalDensity ? 8 : 12
    }

    private var horizontalPadding: CGFloat {
        if usesCompactConversationStyle {
            return 12
        }
        return usesOperationalDensity ? 20 : 16
    }

    private var verticalPadding: CGFloat {
        if usesCompactConversationStyle {
            return 12
        }
        return usesOperationalDensity ? 18 : 16
    }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: stackSpacing) {
                if messages.isEmpty {
                    if historyState.isLoadingInitialHistory {
                        HStack(spacing: 10) {
                            ProgressView()
                            Text("Loading room history…")
                                .foregroundStyle(.secondary)
                        }
                        .font(.footnote)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.vertical, 8)
                    } else if let emptyState {
                        emptyState
                    } else {
                        ChatEmptyStateView(
                            title: "No messages yet",
                            message: "Be the first to say hello."
                        )
                    }
                } else {
                    if scrollDirection == .chat {
                        historyControls
                    }

                    if let errorMessage = historyState.errorMessage {
                        Text(errorMessage)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .center)
                    }

                    ForEach(Array(displayedMessages.enumerated()), id: \.element.id) { index, message in
                        let previousMessage = index > 0 ? displayedMessages[index - 1] : nil
                        let nextMessage = index + 1 < displayedMessages.count ? displayedMessages[index + 1] : nil

                        if usesCompactConversationStyle, message.startsTimelineDay(after: previousMessage) {
                            ChatTimelineDayDividerView(date: message.sentAt)
                                .padding(.top, previousMessage == nil ? 0 : 10)
                        }

                        if message.id == firstUnreadMessageID {
                            ChatUnreadDividerView()
                                .id("unread-divider")
                                .transition(.opacity)
                        }

                        ChatMessageRowView(
                            message: message,
                            previousMessage: previousMessage,
                            nextMessage: nextMessage,
                            usesCompactConversationStyle: usesCompactConversationStyle,
                            threadChildCount: childrenByThreadID[message.id]?.count ?? 0,
                            onReply: onReply,
                            onRetract: onRetract,
                            onOpenThread: onOpenThread,
                            avatarData: avatarDataBySenderID(message.senderID),
                            onRequestAvatar: onRequestAvatar
                        )
                        .id(message.id)
                        .transition(.asymmetric(
                            insertion: .opacity.combined(with: .move(edge: .bottom)),
                            removal: .opacity
                        ))
                    }

                    if scrollDirection == .social {
                        historyControls
                    }
                }
            }
            .frame(maxWidth: usesOperationalDensity ? 860 : .infinity, alignment: .leading)
            .padding(.horizontal, horizontalPadding)
            .padding(.vertical, verticalPadding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .animation(.smooth(duration: 0.22), value: displayedMessages.map(\.id))
        }
        .background(WaddleTheme.chatBackground)
        #if os(iOS)
        // Dismiss the software keyboard when the user drags the message list
        // or taps anywhere outside the composer/keyboard. `.immediately` hides
        // the keyboard as soon as scrolling starts; the simultaneous tap
        // gesture covers the static-tap case without consuming taps meant for
        // message rows (reactions, reply buttons, links).
        .scrollDismissesKeyboard(.immediately)
        .simultaneousGesture(
            TapGesture().onEnded {
                UIApplication.shared.sendAction(
                    #selector(UIResponder.resignFirstResponder),
                    to: nil, from: nil, for: nil
                )
            }
        )
        #endif
    }

    @ViewBuilder
    private var historyControls: some View {
        if historyState.isLoadingOlderMessages {
            HStack(spacing: 10) {
                ProgressView()
                Text("Loading older messages…")
                    .foregroundStyle(.secondary)
            }
            .font(.footnote)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.vertical, 8)
        } else if historyState.canLoadOlderMessages, let onLoadOlderMessages {
            Button(action: onLoadOlderMessages) {
                Label("Load older messages", systemImage: "arrow.up.circle")
                    .font(.footnote.weight(.medium))
            }
            .buttonStyle(.bordered)
            .frame(maxWidth: .infinity, alignment: .center)
        }
    }
}

/// Thin "New" rail placed directly above the first unread message in a room.
/// Uses a warning-tinted hairline with a small pill label on the trailing
/// edge so it reads as "you haven't seen below this line" without competing
/// with day dividers.
struct ChatUnreadDividerView: View {
    var body: some View {
        HStack(spacing: 8) {
            VStack { Divider().overlay(WaddleTheme.unreadBadge) }
            Text("New")
                .font(.caption2.weight(.bold))
                .foregroundStyle(.white)
                .padding(.horizontal, 8)
                .padding(.vertical, 2)
                .background(WaddleTheme.unreadBadge, in: Capsule())
        }
        .padding(.top, 10)
        .padding(.bottom, 4)
        .padding(.horizontal, 16)
    }
}

struct ChatTimelineDayDividerView: View {
    let date: Date

    private var label: String {
        if Calendar.current.isDateInToday(date) { return "Today" }
        if Calendar.current.isDateInYesterday(date) { return "Yesterday" }
        return date.formatted(.dateTime.weekday(.wide).month(.abbreviated).day())
    }

    var body: some View {
        // Centred pill flanked by hairlines — the Slack/Discord pattern that
        // reads as "new day" without dominating the scroll view.
        HStack(spacing: 10) {
            VStack { Divider().overlay(WaddleTheme.divider) }
            Text(label)
                .font(.caption.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
                .padding(.horizontal, 10)
                .padding(.vertical, 3)
                .background(
                    WaddleTheme.surfaceRaised.opacity(0.8),
                    in: Capsule()
                )
                .overlay(
                    Capsule()
                        .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
                )
            VStack { Divider().overlay(WaddleTheme.divider) }
        }
        .padding(.top, 18)
        .padding(.bottom, 6)
        .padding(.horizontal, 16)
    }
}

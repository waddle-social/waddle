import SwiftUI

struct ChatConversationHeaderView: View {
    let room: ChatRoomSelection?
    let bannerState: ChatConnectionBannerState
    let memberCount: Int
    var messageCount: Int = 0
    var showsMemberButton: Bool = false
    var onShowMembers: (() -> Void)? = nil
    var usesOperationalChrome: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: bannerState.isVisible ? 10 : 0) {
            HStack(alignment: .center, spacing: 10) {
                if usesOperationalChrome {
                    Image(systemName: "number")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(WaddleTheme.accent)
                }

                Text(usesOperationalChrome ? (room?.title ?? "chat") : (room?.title ?? "Chat"))
                    .font(usesOperationalChrome ? .system(size: 17, weight: .semibold) : .title2.weight(.semibold))

                if let room, room.isMuted {
                    headerPill(title: "Muted", systemImage: "bell.slash.fill", tint: .secondary)
                }

                Spacer(minLength: 12)

                if usesOperationalChrome {
                    headerPill(
                        title: memberCount == 1 ? "1 member" : "\(memberCount) members",
                        systemImage: "person.2.fill",
                        tint: .secondary
                    )

                    if messageCount > 0 {
                        headerPill(
                            title: "\(messageCount)",
                            systemImage: "bubble.left.fill",
                            tint: .secondary
                        )
                    }
                }

                if showsMemberButton, let onShowMembers {
                    if usesOperationalChrome {
                        Button(action: onShowMembers) {
                            Image(systemName: "person.2")
                                .font(.system(size: 13, weight: .semibold))
                                .frame(width: 32, height: 32)
                                .background(
                                    WaddleTheme.surfaceRaised,
                                    in: RoundedRectangle(cornerRadius: 10, style: .continuous)
                                )
                                .overlay {
                                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                                        .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                                }
                        }
                        .buttonStyle(.plain)
                    } else {
                        Button(action: onShowMembers) {
                            Label("Members", systemImage: "person.2.fill")
                                .font(.footnote.weight(.medium))
                        }
                        .buttonStyle(.bordered)
                    }
                }
            }

            if let subtitle = room?.subtitle, !subtitle.isEmpty {
                Text(subtitle)
                    .foregroundStyle(WaddleTheme.textSecondary)
                    .font(usesOperationalChrome ? .system(size: 12, weight: .medium) : .footnote)
                    .lineLimit(2)
            }

            if bannerState.isVisible {
                ChatConnectionBannerView(state: bannerState, usesOperationalChrome: usesOperationalChrome)
            }
        }
        .padding(.horizontal, usesOperationalChrome ? 18 : 16)
        .padding(.top, usesOperationalChrome ? 14 : 16)
        .padding(.bottom, usesOperationalChrome ? 12 : 16)
        .background(headerBackground)
        .overlay(alignment: .bottom) {
            if usesOperationalChrome {
                Rectangle()
                    .fill(WaddleTheme.divider)
                    .frame(height: 1)
            }
        }
    }

    @ViewBuilder
    private func headerMetaLabel(
        systemImage: String,
        text: String,
        tint: Color = .secondary,
        emphasized: Bool = false
    ) -> some View {
        Label {
            Text(text)
        } icon: {
            Image(systemName: systemImage)
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(tint)
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background(tint.opacity(emphasized ? 0.12 : 0.10), in: Capsule())
    }

    @ViewBuilder
    private func headerPill(title: String, systemImage: String?, tint: Color) -> some View {
        HStack(spacing: 5) {
            if let systemImage {
                Image(systemName: systemImage)
            }
            Text(title)
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(tint.opacity(0.12), in: Capsule())
    }

    @ViewBuilder
    private var headerBackground: some View {
        if usesOperationalChrome {
            WaddleTheme.chatBackground
        } else {
            Color.clear
        }
    }
}

struct ChatConnectionBannerView: View {
    let state: ChatConnectionBannerState
    var usesOperationalChrome: Bool = false

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: state.symbolName)
                .font(.caption.weight(.semibold))
                .foregroundStyle(tint)
            Text(state.message)
                .font(.footnote.weight(.medium))
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, usesOperationalChrome ? 14 : 12)
        .padding(.vertical, usesOperationalChrome ? 10 : 8)
        .background(tint.opacity(usesOperationalChrome ? 0.08 : 0.10), in: RoundedRectangle(cornerRadius: usesOperationalChrome ? 14 : 999, style: .continuous))
    }

    private var tint: Color {
        switch state {
        case .connected:
            return .green
        case .connecting, .reconnecting:
            return .blue
        case .disconnected:
            return .orange
        case .error:
            return .red
        case .hidden:
            return .secondary
        }
    }
}

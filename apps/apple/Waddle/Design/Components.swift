import SwiftUI
import WaddleKit

/// XEP-0084 avatar with an initials fallback in the XEP-0392 color.
struct AvatarView: View {
    let name: String
    /// Identifier hashed for the fallback color (bare JID or nick).
    let colorKey: String
    var image: AvatarImage?
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        Group {
            if let image, let decoded = Image(data: image.data) {
                decoded
                    .resizable()
                    .scaledToFill()
            } else {
                ZStack {
                    Color.consistent(for: colorKey)
                    Text(initials)
                        .font(.system(size: size * 0.4, weight: .semibold, design: .rounded))
                        .foregroundStyle(.white)
                }
            }
        }
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: size * 0.3, style: .continuous))
        .accessibilityHidden(true)
    }

    private var initials: String {
        let parts = name.split(whereSeparator: { $0 == " " || $0 == "." || $0 == "_" || $0 == "-" }).prefix(2)
        let letters = parts.compactMap(\.first).map(String.init).joined()
        return letters.isEmpty ? "?" : letters.uppercased()
    }
}

/// Avatar for a JID, fetched through the session's avatar store.
struct JIDAvatar: View {
    @Environment(SessionCoordinator.self) private var session
    let jid: BareJID
    var name: String?
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        AvatarView(
            name: name ?? jid.localpart ?? jid.domain,
            colorKey: jid.description,
            image: session.avatars.image(for: jid),
            size: size
        )
        .task(id: jid) { session.loadAvatarIfNeeded(jid) }
    }
}

/// RFC 6121 availability dot.
struct PresenceDot: View {
    let availability: Availability
    var size: CGFloat = 10

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .overlay(Circle().strokeBorder(Color.primaryBackground, lineWidth: size * 0.2))
            .accessibilityLabel(Text(label))
    }

    private var color: Color {
        switch availability {
        case .available, .chat: return .green
        case .away, .extendedAway: return .orange
        case .doNotDisturb: return .red
        case .offline: return .gray.opacity(0.6)
        }
    }

    private var label: String {
        switch availability {
        case .available, .chat: return "Online"
        case .away: return "Away"
        case .extendedAway: return "Away for a while"
        case .doNotDisturb: return "Do not disturb"
        case .offline: return "Offline"
        }
    }
}

/// Unread count capsule; the mention variant is filled with the accent.
struct UnreadBadge: View {
    let count: Int
    var isMention = false

    var body: some View {
        if count > 0 {
            Text(count > 99 ? "99+" : "\(count)")
                .font(.caption2.weight(.bold).monospacedDigit())
                .foregroundStyle(isMention ? Color.white : Color.primary)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(
                    Capsule().fill(isMention ? Color.accentColor : Color.secondary.opacity(0.22))
                )
                .accessibilityLabel(Text(isMention ? "\(count) unread, mentions you" : "\(count) unread"))
        }
    }
}

/// Offline / reconnecting strip shown above the content.
struct ConnectionBanner: View {
    let status: ConnectionStatus

    var body: some View {
        if let content {
            HStack(spacing: Theme.Spacing.s) {
                if content.showsProgress {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: content.symbol)
                }
                Text(content.text)
                    .font(.footnote.weight(.medium))
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.vertical, Theme.Spacing.s)
            .foregroundStyle(content.tint)
            .background(content.tint.opacity(0.12))
            .transition(.move(edge: .top).combined(with: .opacity))
            .accessibilityElement(children: .combine)
        }
    }

    private var content: (text: String, symbol: String, tint: Color, showsProgress: Bool)? {
        switch status {
        case .online, .signedOut:
            return nil
        case .connecting:
            return ("Connecting…", "arrow.triangle.2.circlepath", .secondary, true)
        case let .offline(retryAt):
            if let retryAt, retryAt > Date() {
                return ("Offline. Retrying \(retryAt.formatted(.relative(presentation: .named)))", "wifi.slash", .orange, false)
            }
            return ("Offline. Reconnecting…", "wifi.slash", .orange, false)
        case .authenticationFailed:
            return ("Your session expired. Sign in again.", "person.crop.circle.badge.exclamationmark", .red, false)
        }
    }
}

/// Centered empty or error state.
struct EmptyStateView: View {
    let title: String
    let message: String
    let symbol: String

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: symbol)
        } description: {
            Text(message)
        }
    }
}

struct WaddleBrandMark: View {
    var size: CGFloat = 44

    var body: some View {
        Image("WaddleLogo")
            .resizable()
            .interpolation(.high)
            .scaledToFit()
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}
